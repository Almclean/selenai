use std::{
    env,
    fmt::Write as _,
    io::{self, Stdout},
    sync::{Arc, mpsc as std_mpsc},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use serde_json::to_string_pretty;
use tokio::{runtime::Runtime, sync::mpsc};
use unicode_width::UnicodeWidthStr;

use crate::{
    config::{AppConfig, ProviderKind},
    llm::{
        ChatRequest, ChatResponse, LlmClient, LlmTool, StreamEvent, StubClient,
        openai::{OpenAiClient, OpenAiConfig},
    },
    lua_tool::{LuaExecution, LuaExecutor},
    tui,
    types::{Message, Role, ToolInvocation, ToolLogEntry, ToolStatus},
};

const LLM_LUA_TOOL_NAME: &str = "lua_run_script";

pub struct App {
    config: AppConfig,
    state: AppState,
    llm: Arc<dyn LlmClient>,
    runtime: Runtime,
    lua: LuaExecutor,
    should_quit: bool,
    next_tool_id: usize,
    active_stream: Option<ActiveStream>,
    pending_lua_tools: Vec<PendingLuaTool>,
}

impl App {
    pub fn new() -> Result<Self> {
        let workspace = env::current_dir().context("failed to get current dir")?;
        let runtime = Runtime::new()?;
        let config = AppConfig::load()?;
        let llm = build_llm_client(&config)?;
        let mut state = AppState::default();
        if !config.allow_tool_writes {
            state.push_message(Message::new(
                Role::Assistant,
                "Lua helpers are running in read-only mode (enable writes in selenai.toml).",
            ));
        }
        let allow_writes = config.allow_tool_writes;
        Ok(Self {
            config,
            state,
            llm,
            runtime,
            lua: LuaExecutor::new(workspace, allow_writes)?,
            should_quit: false,
            next_tool_id: 0,
            active_stream: None,
            pending_lua_tools: Vec::new(),
        })
    }

    pub fn run(&mut self) -> Result<()> {
        let mut stdout = io::stdout();
        enable_raw_mode()?;
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        terminal.hide_cursor()?;

        let result = self.event_loop(&mut terminal);

        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        terminal.show_cursor()?;

        result
    }

    fn event_loop(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
        let tick_rate = Duration::from_millis(150);
        let mut last_tick = Instant::now();

        loop {
            self.poll_active_stream();
            terminal.draw(|frame| tui::draw(frame, &self.state))?;

            if self.should_quit {
                break;
            }

            let timeout = tick_rate
                .checked_sub(last_tick.elapsed())
                .unwrap_or_else(|| Duration::from_secs(0));

            if event::poll(timeout)? {
                let event = event::read()?;
                self.handle_event(event);
            }

            if last_tick.elapsed() >= tick_rate {
                last_tick = Instant::now();
            }
        }

        Ok(())
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.handle_key_event(key),
            Event::Mouse(_) | Event::Resize(_, _) | Event::FocusGained | Event::FocusLost => {}
            Event::Paste(data) => {
                if self.state.focus == FocusTarget::Input && !data.is_empty() {
                    for ch in data.chars() {
                        self.state.input.insert_char(ch);
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_key_event(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => {
                    self.should_quit = true;
                    return;
                }
                KeyCode::Char('l') => {
                    self.state.tool_logs.clear();
                    self.state.tool_scroll = 0;
                    return;
                }
                KeyCode::Char('u') if self.state.focus == FocusTarget::Input => {
                    self.state.input.clear();
                    return;
                }
                KeyCode::Char('b') => {
                    self.state.copy_mode = !self.state.copy_mode;
                    let status = if self.state.copy_mode {
                        "enabled"
                    } else {
                        "disabled"
                    };
                    self.state.push_message(Message::new(
                        Role::Assistant,
                        format!("Copy-friendly mode {status}. Panel borders {status}."),
                    ));
                    return;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Esc => self.should_quit = true,
            KeyCode::Tab => self.state.focus = self.state.focus.next(),
            KeyCode::BackTab => self.state.focus = self.state.focus.prev(),
            KeyCode::Up => self.scroll_active(-1),
            KeyCode::Down => self.scroll_active(1),
            KeyCode::PageUp => self.scroll_active(-5),
            KeyCode::PageDown => self.scroll_active(5),
            KeyCode::Enter if self.state.focus == FocusTarget::Input => self.submit_current_input(),
            _ => {
                if self.state.focus == FocusTarget::Input {
                    self.handle_input_key(key);
                }
            }
        }
    }

    fn handle_input_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.state.input.insert_char(ch);
            }
            KeyCode::Backspace => {
                self.state.input.backspace();
            }
            KeyCode::Delete => {
                self.state.input.delete_char();
            }
            KeyCode::Left => {
                self.state.input.move_left();
            }
            KeyCode::Right => {
                self.state.input.move_right();
            }
            KeyCode::Home => self.state.input.move_to_start(),
            KeyCode::End => self.state.input.move_to_end(),
            _ => {}
        }
    }

    fn scroll_active(&mut self, delta: i16) {
        match self.state.focus {
            FocusTarget::Chat => adjust_chat_scroll(&mut self.state.chat_scroll, delta),
            FocusTarget::Tool => adjust_scroll(&mut self.state.tool_scroll, delta),
            FocusTarget::Input => {}
        }
    }

    fn submit_current_input(&mut self) {
        let current = self.state.input.buffer();
        if current.trim().is_empty() {
            return;
        }

        if self.active_stream.is_some() {
            self.state.push_message(Message::new(
                Role::Assistant,
                "Hang on, I'm still finishing the previous response.",
            ));
            return;
        }

        let text = self.state.input.take();
        self.state
            .push_message(Message::new(Role::User, text.clone()));

        if let Some(command) = parse_tool_command(&text) {
            self.handle_tool_command(command);
        } else if let Some(script) = parse_lua_command(&text) {
            self.invoke_lua(script);
        } else {
            self.invoke_llm();
        }
    }

    fn invoke_llm(&mut self) {
        let system_prompt = self.build_system_prompt();
        let lua_tool = self.build_lua_tool();
        let mut request = ChatRequest::new(self.state.messages.clone())
            .with_system_prompt(system_prompt)
            .with_tool(lua_tool);
        if self.config.streaming {
            request = request.with_stream(true);
        }

        if self.config.streaming && self.llm.supports_streaming() {
            self.invoke_llm_streaming(request);
        } else {
            self.invoke_llm_unary(request);
        }
    }

    fn invoke_llm_unary(&mut self, request: ChatRequest) {
        let response = self.runtime.block_on(self.llm.chat(request));
        match response {
            Ok(chat_response) => self.handle_chat_response(chat_response),
            Err(err) => self
                .state
                .push_message(Message::new(Role::Assistant, format!("LLM error: {err:#}"))),
        }
    }

    fn invoke_llm_streaming(&mut self, request: ChatRequest) {
        let (tx, rx) = mpsc::unbounded_channel();
        let placeholder_index = self
            .state
            .push_message_with_index(Message::new(Role::Assistant, String::new()));

        let llm = Arc::clone(&self.llm);
        let (result_tx, result_rx) = std_mpsc::channel();

        self.runtime.spawn(async move {
            let result = llm.chat_stream(request, tx).await;
            let _ = result_tx.send(result);
        });

        self.active_stream = Some(ActiveStream {
            receiver: rx,
            result_rx,
            message_index: placeholder_index,
        });
    }

    fn handle_chat_response(&mut self, response: ChatResponse) {
        match response {
            ChatResponse::Assistant(message) => self.state.push_message(message),
            ChatResponse::ToolCall(invocation) => self.handle_tool_call(invocation),
        }
    }

    fn build_system_prompt(&self) -> String {
        let mut prompt = format!(
            "You are SelenAI, a terminal-based AI pair programmer. You can analyze the current repository and MUST aggressively use the `{LLM_LUA_TOOL_NAME}` tool to run Lua code inside a sandboxed helper VM whenever you need information or validation. Do not guess when you can fetch real data from code execution.\n\n\
Key expectations (inspired by Cloudflare's Code Mode and Anthropic's MCP guidance):\n\
- Always outline a brief plan before editing files or running tools.\n\
- Default to the Lua tool for inspecting files, reading configs, summarizing diffs, running tests, or performing calculations; if you choose not to run it, explain why.\n\
- Describe the script you will run, call the tool, then interpret the output before proceeding.\n\
- Keep changes minimal, review results, and avoid destructive edits without explicit confirmation.\n\
- If a run fails, explain what happened and adjust.\n\n"
        );

        if self.config.allow_tool_writes {
            prompt.push_str(
                "The Lua sandbox can read and write within the workspace via helpers like `io.open`, `fs.read`, `rust.read_file`, `rust.list_dir`, `rust.write_file`, `rust.http_request`, and `rust.log`. Use writes only after verifying the plan and results.\n",
            );
        } else {
            prompt.push_str(
                "The Lua sandbox is currently **read-only**. You may use helpers such as `io.open`, `fs.read`, `rust.read_file`, `rust.list_dir`, `rust.http_request`, and `rust.log`, but do not attempt to write files.\n",
            );
        }
        prompt.push_str("If you need third-party Lua helpers, vendor pure-Lua modules inside the workspace (e.g., `lua_libs/foo.lua`) and `load` them via `rust.read_file`—do not attempt global installs.\n");

        prompt.push_str(
            "Be transparent whenever you run code, cite what you executed, and double-check outputs before final answers.",
        );

        prompt
    }

    fn build_lua_tool(&self) -> LlmTool {
        let mut description = format!(
            "Execute Lua code inside the user's workspace using the injected helpers (`io.*`, `fs.*`, and the lower-level `rust.*` functions for read_file, list_dir, write_file, http_request, log, etc.). Use `{LLM_LUA_TOOL_NAME}` when you need to inspect files, gather context, and apply verified edits. Always explain why you need the script and summarize results afterward."
        );
        if !self.config.allow_tool_writes {
            description
                .push_str(" File writes are disabled; limit scripts to read-only inspection.");
        }

        LlmTool::new(
            LLM_LUA_TOOL_NAME,
            description,
            serde_json::json!({
                "type": "object",
                "properties": {
                    "source": {
                        "type": "string",
                        "description": "Lua script to execute. Prefer small, composable scripts."
                    },
                    "reason": {
                        "type": "string",
                        "description": "Short explanation of why this script is being run (plan/verify/apply)."
                    }
                },
                "required": ["source"],
                "additionalProperties": false
            }),
        )
    }

    fn poll_active_stream(&mut self) {
        let Some(mut active) = self.active_stream.take() else {
            return;
        };

        let mut finished = false;
        let mut error_message: Option<String> = None;

        while let Ok(event) = active.receiver.try_recv() {
            match event {
                StreamEvent::Delta(chunk) => {
                    self.state.append_to_message(active.message_index, &chunk);
                }
                StreamEvent::ToolCall(invocation) => self.handle_tool_call(invocation),
                StreamEvent::Completed => {}
            }
        }

        match active.result_rx.try_recv() {
            Ok(result) => {
                finished = true;
                match result {
                    Ok(()) => {
                        if self.state.message_is_empty(active.message_index) {
                            self.state.remove_message(active.message_index);
                        }
                    }
                    Err(err) => {
                        self.state.remove_message(active.message_index);
                        error_message = Some(format!("LLM error: {err:#}"));
                    }
                }
            }
            Err(std_mpsc::TryRecvError::Disconnected) => {
                finished = true;
                self.state.remove_message(active.message_index);
                error_message = Some("LLM stream ended unexpectedly.".to_string());
            }
            Err(std_mpsc::TryRecvError::Empty) => {}
        }

        if finished {
            self.active_stream = None;
        } else {
            self.active_stream = Some(active);
        }

        if let Some(message) = error_message {
            self.state
                .push_message(Message::new(Role::Assistant, message));
        }
    }

    fn invoke_lua(&mut self, script: &str) {
        if script.is_empty() {
            self.state
                .push_message(Message::new(Role::Assistant, "Lua command needs a script."));
            return;
        }

        self.run_lua_script("Lua script", script, None);
    }

    fn run_lua_script(&mut self, title: impl Into<String>, script: &str, call_id: Option<String>) {
        let entry_id = self.create_tool_log_entry(title, script);
        self.execute_lua_entry(entry_id, script, call_id);
    }

    fn create_tool_log_entry(
        &mut self,
        title: impl Into<String>,
        detail: impl Into<String>,
    ) -> usize {
        let entry_id = self.next_tool_id;
        self.next_tool_id += 1;
        let entry = ToolLogEntry::new(entry_id, title, detail);
        self.state.tool_logs.push(entry);
        entry_id
    }

    fn execute_lua_entry(&mut self, entry_id: usize, script: &str, call_id: Option<String>) {
        match self.lua.run_script(script) {
            Ok(output) => {
                let rendered = render_lua_execution(&output);
                self.state.push_message(match call_id.as_deref() {
                    Some(tool_call_id) => Message::new_tool(tool_call_id, rendered.clone()),
                    None => Message::new(Role::Tool, rendered.clone()),
                });
                self.state
                    .update_tool_log(entry_id, ToolStatus::Success, rendered);
            }
            Err(err) => {
                let msg = format!("Lua error: {err:#}");
                self.state.push_message(match call_id.as_deref() {
                    Some(tool_call_id) => Message::new_tool(tool_call_id, msg.clone()),
                    None => Message::new(Role::Tool, msg.clone()),
                });
                self.state.update_tool_log(entry_id, ToolStatus::Error, msg);
            }
        }
    }

    fn handle_tool_call(&mut self, invocation: ToolInvocation) {
        match invocation.name.as_str() {
            LLM_LUA_TOOL_NAME => self.handle_lua_tool(invocation),
            _ => self.state.push_message(render_tool_invocation(invocation)),
        }
    }

    fn handle_lua_tool(&mut self, invocation: ToolInvocation) {
        match LuaToolRequest::from_value(&invocation.arguments) {
            Ok(request) => {
                let mut summary = String::new();
                if let Some(reason) = request.reason.as_deref() {
                    let _ = writeln!(
                        summary,
                        "LLM requested `{LLM_LUA_TOOL_NAME}` with reason:\n{}\n",
                        reason
                    );
                } else {
                    let _ = writeln!(summary, "LLM requested `{LLM_LUA_TOOL_NAME}`.");
                }
                let _ = writeln!(summary, "Script:\n```lua\n{}\n```", request.script);
                if self.config.allow_tool_writes {
                    let _ = writeln!(
                        summary,
                        "Writes are enabled, so this run is queued. Use `/tool run` to approve or `/tool skip` to cancel."
                    );
                } else {
                    let _ = writeln!(summary, "Sandbox is read-only; executing immediately.");
                }
                self.render_tool_summary(summary, &invocation);

                let title = request
                    .reason
                    .as_ref()
                    .map(|r| format!("LLM {LLM_LUA_TOOL_NAME}: {}", truncate_summary(r)))
                    .unwrap_or_else(|| format!("LLM {LLM_LUA_TOOL_NAME}"));
                if self.config.allow_tool_writes {
                    self.queue_lua_tool(title, request, invocation.call_id.clone());
                } else {
                    self.run_lua_script(title, &request.script, invocation.call_id.clone());
                }
            }
            Err(err) => {
                self.state.push_message(Message::new(
                    Role::Assistant,
                    format!("Invalid `{LLM_LUA_TOOL_NAME}` request: {err}"),
                ));
            }
        }
    }

    fn render_tool_summary(&mut self, summary: String, invocation: &ToolInvocation) {
        if let Some(idx) = self.current_stream_message_index() {
            if !self.state.message_is_empty(idx) {
                self.state.append_to_message(idx, "\n");
            }
            self.state.append_to_message(idx, &summary);
            self.state.append_tool_call(idx, invocation.clone());
        } else {
            let mut message = Message::new(Role::Assistant, summary);
            message.tool_calls.push(invocation.clone());
            self.state.push_message(message);
        }
    }

    fn queue_lua_tool(&mut self, title: String, request: LuaToolRequest, call_id: Option<String>) {
        let mut detail = String::new();
        if let Some(reason) = request.reason.as_deref() {
            let _ = writeln!(detail, "Reason: {reason}");
        }
        let _ = writeln!(detail, "Script:\n{}", request.script);
        let entry_id = self.create_tool_log_entry(&title, detail);
        self.pending_lua_tools.push(PendingLuaTool {
            entry_id,
            title,
            script: request.script,
            reason: request.reason,
            call_id,
        });
    }

    fn handle_tool_command(&mut self, command: ToolCommand) {
        match command {
            ToolCommand::RunNext => self.run_pending_tool(None),
            ToolCommand::RunEntry(entry_id) => self.run_pending_tool(Some(entry_id)),
            ToolCommand::SkipNext => self.skip_pending_tool(None),
            ToolCommand::SkipEntry(entry_id) => self.skip_pending_tool(Some(entry_id)),
        }
    }

    fn run_pending_tool(&mut self, entry_id: Option<usize>) {
        if let Some(pending) = self.take_pending_tool(entry_id) {
            let label = pending
                .reason
                .as_ref()
                .map(|r| truncate_summary(r))
                .unwrap_or_else(|| pending.title.clone());
            self.state.push_message(Message::new(
                Role::Assistant,
                format!(
                    "Approved queued {LLM_LUA_TOOL_NAME} (`{label}`) — executing now (entry #{})",
                    pending.entry_id
                ),
            ));
            self.execute_lua_entry(pending.entry_id, &pending.script, pending.call_id);
        } else {
            self.state.push_message(Message::new(
                Role::Assistant,
                &format!("No queued {LLM_LUA_TOOL_NAME} requests to execute."),
            ));
        }
    }

    fn current_stream_message_index(&self) -> Option<usize> {
        self.active_stream
            .as_ref()
            .map(|stream| stream.message_index)
    }

    fn skip_pending_tool(&mut self, entry_id: Option<usize>) {
        if let Some(pending) = self.take_pending_tool(entry_id) {
            let label = pending
                .reason
                .as_ref()
                .map(|r| truncate_summary(r))
                .unwrap_or_else(|| pending.title.clone());
            self.state.update_tool_log(
                pending.entry_id,
                ToolStatus::Error,
                "Canceled before execution.",
            );
            self.state.push_message(Message::new(
                Role::Assistant,
                format!(
                    "Canceled queued {LLM_LUA_TOOL_NAME} (`{label}`) (entry #{})",
                    pending.entry_id
                ),
            ));
        } else {
            self.state.push_message(Message::new(
                Role::Assistant,
                format!("No queued {LLM_LUA_TOOL_NAME} requests to cancel."),
            ));
        }
    }

    fn take_pending_tool(&mut self, entry_id: Option<usize>) -> Option<PendingLuaTool> {
        if let Some(id) = entry_id {
            if let Some(pos) = self
                .pending_lua_tools
                .iter()
                .position(|pending| pending.entry_id == id)
            {
                return Some(self.pending_lua_tools.remove(pos));
            }
            None
        } else if self.pending_lua_tools.is_empty() {
            None
        } else {
            Some(self.pending_lua_tools.remove(0))
        }
    }
}

fn adjust_scroll(scroll: &mut u16, delta: i16) {
    let current = *scroll as i32 + delta as i32;
    *scroll = current.max(0) as u16;
}

fn adjust_chat_scroll(scroll: &mut u16, delta: i16) {
    if delta > 0 {
        // Positive delta scrolls toward the bottom (reduce offset).
        *scroll = scroll.saturating_sub(delta as u16);
    } else if delta < 0 {
        // Negative delta scrolls up (increase offset from the bottom).
        *scroll = scroll.saturating_add((-delta) as u16);
    }
}

fn render_tool_invocation(invocation: ToolInvocation) -> Message {
    let args =
        to_string_pretty(&invocation.arguments).unwrap_or_else(|_| "<unprintable args>".into());
    let content = if let Some(call_id) = invocation.call_id.as_deref() {
        format!(
            "LLM requested tool `{}' (call_id: {}) with arguments:\n{}",
            invocation.name, call_id, args
        )
    } else {
        format!(
            "LLM requested tool `{}' with arguments:\n{}",
            invocation.name, args
        )
    };
    Message::new(Role::Assistant, content)
}

fn render_lua_execution(output: &LuaExecution) -> String {
    let mut content = String::new();
    let _ = writeln!(content, "Lua value:");
    if output.value.is_empty() {
        let _ = writeln!(content, "<empty>");
    } else {
        for line in output.value.split('\n') {
            let _ = writeln!(content, "{line}");
        }
    }

    append_section(&mut content, "Stdout", &output.stdout);
    append_section(&mut content, "Stderr", &output.stderr);
    append_section(&mut content, "Logs", &output.logs);
    content
}

fn append_section(buffer: &mut String, label: &str, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    let _ = writeln!(buffer);
    let _ = writeln!(buffer, "{label}:");
    for line in lines {
        let _ = writeln!(buffer, "{line}");
    }
}

fn build_llm_client(config: &AppConfig) -> Result<Arc<dyn LlmClient>> {
    match config.provider {
        ProviderKind::Stub => Ok(Arc::new(StubClient::new())),
        ProviderKind::OpenAi => {
            let openai_cfg = build_openai_config(config)?;
            let client = OpenAiClient::new(openai_cfg)?;
            Ok(Arc::new(client))
        }
    }
}

fn build_openai_config(config: &AppConfig) -> Result<OpenAiConfig> {
    let openai = &config.openai;
    let api_key = env::var("OPENAI_API_KEY").context(
        "OpenAI provider selected but no API key configured. Set OPENAI_API_KEY (for example in your .env file).",
    )?;
    let base_url = openai
        .base_url
        .clone()
        .or_else(|| env::var("OPENAI_BASE_URL").ok())
        .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
    let organization = openai
        .organization
        .clone()
        .or_else(|| env::var("OPENAI_ORG").ok());
    let project = openai
        .project
        .clone()
        .or_else(|| env::var("OPENAI_PROJECT").ok());

    Ok(OpenAiConfig {
        api_key,
        model: config.model_id.clone(),
        base_url,
        organization,
        project,
    })
}

fn truncate_summary(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "unspecified".to_string();
    }

    const LIMIT: usize = 60;
    let mut result = String::new();
    for (idx, ch) in trimmed.chars().enumerate() {
        if idx >= LIMIT {
            result.push_str("...");
            break;
        }
        result.push(ch);
    }
    result
}

struct LuaToolRequest {
    script: String,
    reason: Option<String>,
}

impl LuaToolRequest {
    fn from_value(value: &serde_json::Value) -> Result<Self, String> {
        let obj = value
            .as_object()
            .ok_or_else(|| "arguments must be an object".to_string())?;
        let source = obj
            .get("source")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "missing `source` string".to_string())?;
        let reason = obj
            .get("reason")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        Ok(Self {
            script: source.to_string(),
            reason,
        })
    }
}

fn parse_lua_command(input: &str) -> Option<&str> {
    let trimmed = input.trim_start();
    if !trimmed.starts_with("/lua") {
        return None;
    }

    let rest = &trimmed[4..];
    if rest.is_empty() {
        return Some("");
    }

    if rest.starts_with(char::is_whitespace) {
        return Some(rest.trim_start());
    }

    None
}

fn parse_tool_command(input: &str) -> Option<ToolCommand> {
    let trimmed = input.trim_start();
    if !trimmed.starts_with("/tool") {
        return None;
    }
    let rest = trimmed[5..].trim();
    if rest.is_empty() {
        return None;
    }
    let mut parts = rest.split_whitespace();
    let action = parts.next()?;
    let action = action.to_lowercase();
    let id = parts.next().and_then(|token| token.parse::<usize>().ok());
    match action.as_str() {
        "run" | "approve" => {
            if let Some(entry_id) = id {
                Some(ToolCommand::RunEntry(entry_id))
            } else {
                Some(ToolCommand::RunNext)
            }
        }
        "skip" | "cancel" => {
            if let Some(entry_id) = id {
                Some(ToolCommand::SkipEntry(entry_id))
            } else {
                Some(ToolCommand::SkipNext)
            }
        }
        _ => None,
    }
}

#[derive(Debug, Clone, Copy)]
enum ToolCommand {
    RunNext,
    RunEntry(usize),
    SkipNext,
    SkipEntry(usize),
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub messages: Vec<Message>,
    pub tool_logs: Vec<ToolLogEntry>,
    pub input: InputState,
    pub focus: FocusTarget,
    /// Number of lines to keep above the latest message (0 = follow bottom).
    pub chat_scroll: u16,
    pub tool_scroll: u16,
    pub copy_mode: bool,
}

impl Default for AppState {
    fn default() -> Self {
        let mut state = Self {
            messages: Vec::new(),
            tool_logs: Vec::new(),
            input: InputState::default(),
            focus: FocusTarget::Input,
            chat_scroll: 0,
            tool_scroll: 0,
            copy_mode: false,
        };
        state.push_message(Message::new(
            Role::Assistant,
            "Welcome to SelenAI — Tab moves focus, Ctrl+C exits.",
        ));
        state
    }
}

impl AppState {
    pub fn push_message(&mut self, message: Message) {
        self.messages.push(message);
        self.chat_scroll = 0;
    }

    pub fn push_message_with_index(&mut self, message: Message) -> usize {
        let index = self.messages.len();
        self.push_message(message);
        index
    }

    pub fn update_tool_log(&mut self, id: usize, status: ToolStatus, detail: impl Into<String>) {
        if let Some(entry) = self.tool_logs.iter_mut().find(|entry| entry.id == id) {
            entry.status = status;
            entry.detail = detail.into();
        }
    }

    pub fn append_to_message(&mut self, index: usize, text: &str) {
        if let Some(message) = self.messages.get_mut(index) {
            message.content.push_str(text);
            self.chat_scroll = 0;
        }
    }

    pub fn append_tool_call(&mut self, index: usize, invocation: ToolInvocation) {
        if let Some(message) = self.messages.get_mut(index) {
            message.tool_calls.push(invocation);
        }
    }

    pub fn message_is_empty(&self, index: usize) -> bool {
        self.messages
            .get(index)
            .map(|message| message.content.is_empty())
            .unwrap_or(true)
    }

    pub fn remove_message(&mut self, index: usize) {
        if index < self.messages.len() {
            self.messages.remove(index);
            self.chat_scroll = 0;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusTarget {
    Chat,
    Tool,
    Input,
}

impl FocusTarget {
    pub fn next(self) -> Self {
        match self {
            FocusTarget::Chat => FocusTarget::Tool,
            FocusTarget::Tool => FocusTarget::Input,
            FocusTarget::Input => FocusTarget::Chat,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            FocusTarget::Chat => FocusTarget::Input,
            FocusTarget::Tool => FocusTarget::Chat,
            FocusTarget::Input => FocusTarget::Tool,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct InputState {
    buffer: String,
    cursor: usize, // cursor position in characters
}

impl InputState {
    pub fn buffer(&self) -> String {
        self.buffer.clone()
    }

    pub fn insert_char(&mut self, ch: char) {
        let idx = self.byte_index(self.cursor);
        self.buffer.insert(idx, ch);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self.byte_index(self.cursor - 1);
        let end = self.byte_index(self.cursor);
        self.buffer.replace_range(start..end, "");
        self.cursor -= 1;
    }

    pub fn delete_char(&mut self) {
        if self.cursor >= self.len_chars() {
            return;
        }
        let start = self.byte_index(self.cursor);
        let end = self.byte_index(self.cursor + 1);
        self.buffer.replace_range(start..end, "");
    }

    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.len_chars() {
            self.cursor += 1;
        }
    }

    pub fn move_to_start(&mut self) {
        self.cursor = 0;
    }

    pub fn move_to_end(&mut self) {
        self.cursor = self.len_chars();
    }

    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.buffer)
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
    }

    pub fn cursor_display_offset(&self) -> u16 {
        let idx = self.byte_index(self.cursor);
        let slice = &self.buffer[..idx];
        UnicodeWidthStr::width(slice) as u16
    }

    fn len_chars(&self) -> usize {
        self.buffer.chars().count()
    }

    fn byte_index(&self, cursor: usize) -> usize {
        if cursor == 0 {
            return 0;
        }

        if cursor >= self.len_chars() {
            return self.buffer.len();
        }

        self.buffer
            .char_indices()
            .nth(cursor)
            .map(|(idx, _)| idx)
            .unwrap_or_else(|| self.buffer.len())
    }
}

struct PendingLuaTool {
    entry_id: usize,
    title: String,
    script: String,
    reason: Option<String>,
    call_id: Option<String>,
}

struct ActiveStream {
    receiver: mpsc::UnboundedReceiver<StreamEvent>,
    result_rx: std_mpsc::Receiver<Result<()>>,
    message_index: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lua_tool_request_parses_fields() {
        let value = serde_json::json!({
            "source": "return rust.read_file('Cargo.toml')",
            "reason": "inspect manifest"
        });
        let parsed = LuaToolRequest::from_value(&value).expect("parsed");
        assert_eq!(parsed.script, "return rust.read_file('Cargo.toml')");
        assert_eq!(parsed.reason.as_deref(), Some("inspect manifest"));
    }

    #[test]
    fn lua_tool_request_missing_source_errors() {
        let value = serde_json::json!({ "reason": "missing source" });
        assert!(LuaToolRequest::from_value(&value).is_err());
    }

    #[test]
    fn truncate_summary_limits_length() {
        let text = "a".repeat(80);
        let result = truncate_summary(&text);
        assert_eq!(result.len(), 63); // 60 chars + "..."
        assert!(result.ends_with("..."));
    }

    #[test]
    fn tool_command_parse_run_with_id() {
        match parse_tool_command("/tool run 7") {
            Some(ToolCommand::RunEntry(7)) => {}
            other => panic!("unexpected parse result: {other:?}"),
        }
    }

    #[test]
    fn tool_command_parse_skip_default() {
        match parse_tool_command(" /tool skip ") {
            Some(ToolCommand::SkipNext) => {}
            other => panic!("unexpected parse result: {other:?}"),
        }
    }
}
