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
    macros::MacroConfig,
    session::SessionRecorder,
    tui,
    types::{MarketContext, Message, Role, ToolInvocation, ToolLogEntry, ToolStatus},
};

use tracing::{info, instrument, warn};

const LLM_LUA_TOOL_NAME: &str = "lua_run_script";

#[derive(Debug, PartialEq)]
enum LuaAction<'a> {
    Run(&'a str),
    Reset,
}

pub struct App {
    config: AppConfig,
    macros: MacroConfig,
    state: AppState,
    llm: Arc<dyn LlmClient>,
    runtime: Runtime,
    lua: LuaExecutor,
    session: SessionRecorder,
    should_quit: bool,
    next_tool_id: usize,
    active_stream: Option<ActiveStream>,
    pending_lua_tools: Vec<PendingLuaTool>,
}

impl App {
    pub fn new() -> Result<Self> {
        let workspace = env::current_dir().context("failed to get current dir")?;
        let runtime = Runtime::new()?;
        let handle = runtime.handle().clone();
        let config = AppConfig::load()?;
        let macros = MacroConfig::load()?;
        let llm = build_llm_client(&config)?;
        let mut state = AppState::default();
        if !config.allow_tool_writes {
            state.push_message(Message::new(
                Role::Assistant,
                "Lua helpers are running in read-only mode (enable writes in selenai.toml).",
            ));
        }
        let allow_writes = config.allow_tool_writes;
        let log_root = config.resolve_log_dir(&workspace);
        let session = SessionRecorder::new(&log_root, config.allow_tool_writes)?;
        state.push_message(Message::new(
            Role::Assistant,
            format!(
                "Session transcripts + tool logs will be saved under {}.",
                session.session_dir().display()
            ),
        ));
        
        let mut app = Self {
            config,
            macros,
            state,
            llm,
            runtime,
            lua: LuaExecutor::new(workspace, allow_writes, handle)?,
            session,
            should_quit: false,
            next_tool_id: 0,
            active_stream: None,
            pending_lua_tools: Vec::new(),
        };
        
        app.check_first_run();
        Ok(app)
    }
    
    fn check_first_run(&mut self) {
        let home = env::var("HOME").unwrap_or_else(|_| ".".into());
        let marker = std::path::Path::new(&home).join(".config/selenai/.seen_tour");
        
        if !marker.exists() {
             // Ensure directory exists
             if let Some(parent) = marker.parent() {
                 let _ = std::fs::create_dir_all(parent);
             }
             // Create marker
             let _ = std::fs::write(&marker, "");
             
             self.state.push_message(Message::new(Role::Assistant, 
                 "👋 **Welcome to SelenAI!** It looks like your first time here.\n\n\
                  I am your terminal-based AI pair programmer. Here's a quick tour:\n\
                  1. **Chat**: Type here to talk to me. I can read files, run tests, and edit code.\n\
                  2. **Tools**: I execute Lua scripts to interact with your system. You'll see my plans and outputs in the right pane.\n\
                  3. **Safety**: By default, I might be Read-Only. Check `/config show`.\n\
                  4. **Commands**: Try `/review` to check git changes, or `/help` (conceptually) for more.\n\
                  \n\
                  Start by asking me to \"analyze the current project structure\"!"
             ));
        }
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

        let persist_result = self
            .session
            .persist(&self.state.messages, &self.state.tool_logs);

        result.and(persist_result)
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
            KeyCode::Left if self.state.focus == FocusTarget::Tool => {
                self.state.active_tab = self.state.active_tab.next();
            }
            KeyCode::Right if self.state.focus == FocusTarget::Tool => {
                self.state.active_tab = self.state.active_tab.next();
            }
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
            FocusTarget::Tool => adjust_chat_scroll(&mut self.state.tool_scroll, delta),
            FocusTarget::Input => {}
        }
    }

    fn submit_current_input(&mut self) {
        let mut current = self.state.input.buffer();
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
        
        // Macro expansion
        if current.starts_with('@') {
            let key = current[1..].trim();
            if let Some(expanded) = self.macros.macros.get(key) {
                current = expanded.clone();
                self.state.input.clear(); // clear input manually as we consumed it
            } else {
                 // Unknown macro, treat as literal or error?
                 // Let's treat as literal for now, or warn.
            }
        } else {
            self.state.input.clear();
        }
        
        let text = current; // input is now cleared or expanded

        self.state
            .push_message(Message::new(Role::User, text.clone()));

        if let Some(command) = parse_tool_command(&text) {
            self.handle_tool_command(command);
        } else if let Some(action) = parse_lua_command(&text) {
            self.invoke_lua(action);
        } else if let Some(target) = parse_review_command(&text) {
             self.handle_review_command(target);
        } else if let Some(ticker) = parse_context_command(&text) {
             self.handle_context_command(ticker);
        } else if let Some((action, key, val)) = parse_config_command(&text) {
             self.handle_config_command(action, key, val);
        } else {
            self.invoke_llm();
        }
    }

    fn handle_context_command(&mut self, ticker: &str) {
        if ticker.is_empty() {
             self.state.push_message(Message::new(Role::Assistant, "Usage: /context <TICKER>"));
             return;
        }

        let script = format!(
            r#"
            local quote = rust.get_quote("{ticker}")
            rust.set_context({{
                active_ticker = "{ticker}",
                price = quote.price,
                change_percent = 0.0,
                headlines = {{ "Manually set context to " .. "{ticker}" }},
                technical_summary = "Loaded via /context"
            }})
            return "Context updated to " .. "{ticker}"
            "#
        );

        self.state.push_message(Message::new(Role::User, format!("/context {ticker}")));
        self.run_lua_script(format!("Fetch context for {ticker}"), &script, None);
    }

    fn handle_review_command(&mut self, target: &str) {
        let script = format!(
            r#"
            local status = rust.git_status().stdout
            if status == "" and "{target}" == "" then
                return "Working tree clean, nothing to review."
            end
            
            local diff_cmd = {{ "diff" }}
            if "{target}" ~= "" then
                table.insert(diff_cmd, "{target}")
            end
            
            local diff = rust.run_command("git", diff_cmd).stdout
            if diff == "" then
                return "No changes found for review."
            end
            
            return "Here is the diff for review:\n" .. diff
            "#
        );
        
        let plan = format!("Reviewing changes in `{target}` (or staged/working if empty).");
        self.state.push_message(Message::new(Role::User, format!("/review {target}")));
        self.run_lua_script(plan, &script, None);
    }

    fn handle_config_command(&mut self, action: &str, key: Option<&str>, val: Option<&str>) {
        match action {
            "show" => {
                let display = format!("{:#?}", self.config);
                self.state.push_message(Message::new(Role::Assistant, format!("Current Config:\n```\n{display}\n```")));
            }
            "set" => {
                 if let Some(k) = key {
                     if k == "allow_tool_writes" {
                         if let Some(v) = val {
                             let new_val = v == "true";
                             self.config.allow_tool_writes = new_val;
                             
                             // Simple fix: recreate.
                             let handle = self.runtime.handle().clone();
                             match LuaExecutor::new(env::current_dir().unwrap(), new_val, handle) {
                                 Ok(executor) => {
                                     self.lua = executor;
                                     self.state.push_message(Message::new(Role::Assistant, format!("Config `{k}` set to `{new_val}`.")));
                                 }
                                 Err(e) => {
                                     self.state.push_message(Message::new(Role::Assistant, format!("Failed to update config: {e}")));
                                 }
                             }
                         } else {
                             self.state.push_message(Message::new(Role::Assistant, "Missing value (true/false)."));
                         }
                     } else {
                         self.state.push_message(Message::new(Role::Assistant, format!("Unknown config key `{k}`. Supported: allow_tool_writes")));
                     }
                 } else {
                     self.state.push_message(Message::new(Role::Assistant, "Missing key."));
                 }
            }
            _ => {}
        }
    }

    #[instrument(skip(self))]
    fn invoke_llm(&mut self) {
        let system_prompt = Self::build_system_prompt(&self.config);
        let lua_tool = Self::build_lua_tool(&self.config);
        let mut request = ChatRequest::new(self.state.messages.clone())
            .with_system_prompt(system_prompt)
            .with_tool(lua_tool);
        if self.config.streaming {
            request = request.with_stream(true);
        }

        info!("invoking LLM (streaming={})", self.config.streaming);

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

    #[instrument(skip(self))]
    fn handle_chat_response(&mut self, response: ChatResponse) {
        match response {
            ChatResponse::Assistant(message) => {
                 info!("received assistant message: {} chars", message.content.len());
                 self.state.push_message(message);
            }
            ChatResponse::ToolCalls(invocations) => {
                for invocation in invocations {
                    self.handle_tool_call(invocation);
                }
            }
        }
    }

    fn build_system_prompt(config: &AppConfig) -> String {
        let mut prompt = format!(
            r#"You are SelenAI, an advanced AI software engineer running in a CLI.
Your primary method of interaction is the `{LLM_LUA_TOOL_NAME}` tool, which executes Lua code in a persistent environment.

## Core Philosophy
1. **Reasoning First**: Always analyze the request and state your plan before writing code.
2. **Code as Action**: You do not just "talk" about code; you write Lua scripts to *do* things (explore, read, test, modify).
3. **Persistence**: The Lua state is preserved. You can define functions or variables in one turn and use them in the next.

## The Lua Environment
- **Stdlib**: Standard Lua 5.4 (math, table, string, etc.).
- **Helpers**: `repr(obj)` (inspect data), `print(...)` (output), `warn(...)` (log to stderr).
- **Rust API (`rust` table)**:
  - `rust.list_dir(path)` -> table of `{{name, is_dir}}`
  - `rust.read_file(path)` -> string
  - `rust.search(pattern, dir?)` -> `{{stdout, stderr, status}}` (Recursive grep)
  - `rust.git_status()` -> `{{stdout, status}}`
  - `rust.http_request({{url=..., method=..., headers=..., body=...}})` -> `{{status, body, headers}}`
  - `rust.get_quote(ticker)` -> `{{price, high, low, volume, timestamp}}`
  - `rust.set_context(table)` -> nil (Updates dashboard with {{active_ticker, price, etc.}})
  - `rust.env(key)` -> string or nil
"#
        );

        if config.allow_tool_writes {
            prompt.push_str(
                r#"  - `rust.write_file(path, content)` -> nil
  - `rust.patch_file(path, unified_diff)` -> nil (Preferred for small edits)
  - `rust.run_command(cmd, {args...})` -> `{status, stdout, stderr}`

## Safety & Permissions
- **Write Mode**: ENABLED. You can modify files and run commands.
- **Verification**: Always verify your changes by reading the file back or running a test after modification.
- **Approval**: Tool calls with side effects are paused for user approval. Explain your changes clearly.
"#,
            );
        } else {
            prompt.push_str(
                r#"  - **Note**: `write_file`, `patch_file`, and `run_command` are currently **DISABLED** (Read-Only Mode).

## Safety & Permissions
- **Write Mode**: READ-ONLY. You cannot modify files or run commands.
- Focus on analysis, debugging, and explaining the code.
"#,
            );
        }

        prompt.push_str(
            r#"
## Usage Patterns
- **Exploration**: `local files = rust.list_dir("."); print(repr(files))`
- **Searching**: `print(rust.search("TODO", "src").stdout)`
- **Editing**:
  1. Read file: `local src = rust.read_file("main.rs")`
  2. Plan change: "I need to change X to Y..."
  3. Apply: `rust.patch_file("main.rs", diff_string)` OR `rust.write_file("main.rs", new_content)`
- **Testing**: `local res = rust.run_command("cargo", {"test"}); print(res.stdout)`

## Instructions
- **Think** before you act. Break complex tasks into steps.
- **Use Lua** for logic. If you need to filter a list or parse data, write a script to do it.
- **Output Results**: Use `print()` to show the user the result of your script.
- **Context Awareness**: If the user asks about a stock, use `rust.get_quote` and `rust.set_context` to update the dashboard.
"#,
        );

        prompt
    }

    fn build_lua_tool(config: &AppConfig) -> LlmTool {
        let mut description = format!(
            "Execute Lua code inside the user's workspace using the injected helpers (`io.*`, `fs.*`, and the lower-level `rust.*` functions for read_file, list_dir, write_file, http_request, log, etc.). Use `{LLM_LUA_TOOL_NAME}` when you need to inspect files, gather context, and apply verified edits. Always explain why you need the script and summarize results afterward."
        );
        if !config.allow_tool_writes {
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

    #[instrument(skip(self))]
    fn invoke_lua(&mut self, action: LuaAction) {
        match action {
            LuaAction::Run(script) => {
                if script.is_empty() {
                    self.state
                        .push_message(Message::new(Role::Assistant, "Lua command needs a script."));
                    return;
                }
                self.run_lua_script("Lua script", script, None);
            }
            LuaAction::Reset => {
                match self.lua.reset() {
                    Ok(()) => {
                         self.state.push_message(Message::new(Role::Assistant, "Lua environment reset. Global variables cleared."));
                    }
                    Err(e) => {
                         self.state.push_message(Message::new(Role::Assistant, format!("Failed to reset Lua environment: {e}")));
                    }
                }
            }
        }
    }

    #[instrument(skip(self))]
    fn run_lua_script(&mut self, title: impl Into<String> + std::fmt::Debug, script: &str, call_id: Option<String>) {
        let title_str = title.into();
        let entry_id = self.create_tool_log_entry(&title_str, script);
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
        self.state.tool_scroll = 0;
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

                for update in output.dashboard_updates {
                     if let Ok(ctx) = serde_json::from_str::<MarketContext>(&update) {
                         self.state.market_context = ctx;
                         self.state.active_tab = RightPanelTab::MarketData;
                     }
                }
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

    #[instrument(skip(self))]
    fn handle_tool_call(&mut self, invocation: ToolInvocation) {
        info!(tool = invocation.name, "handling tool call");
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
        
        // Generate preview of side effects (e.g. patches, writes)
        match self.lua.preview_script(&request.script) {
            Ok(preview) => {
                let _ = writeln!(detail, "\n--- PREVIEW ---\n{}", preview);
            }
            Err(err) => {
                 let _ = writeln!(detail, "\n--- PREVIEW ERROR ---\nFailed to generate preview: {err}");
            }
        }

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
                format!("No queued {LLM_LUA_TOOL_NAME} requests to execute."),
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

fn parse_lua_command(input: &str) -> Option<LuaAction> {
    let trimmed = input.trim_start();
    if !trimmed.starts_with("/lua") {
        return None;
    }

    let rest = &trimmed[4..];
    if rest.trim() == "reset" {
        return Some(LuaAction::Reset);
    }
    
    if rest.is_empty() {
        return Some(LuaAction::Run(""));
    }

    if rest.starts_with(char::is_whitespace) {
        return Some(LuaAction::Run(rest.trim_start()));
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

fn parse_review_command(input: &str) -> Option<&str> {
    let trimmed = input.trim_start();
    if !trimmed.starts_with("/review") {
        return None;
    }
    let rest = trimmed[7..].trim();
    Some(rest)
}

fn parse_context_command(input: &str) -> Option<&str> {
    let trimmed = input.trim_start();
    if !trimmed.starts_with("/context") {
        return None;
    }
    let rest = trimmed[8..].trim();
    Some(rest)
}

fn parse_config_command(input: &str) -> Option<(&str, Option<&str>, Option<&str>)> {
    let trimmed = input.trim_start();
    if !trimmed.starts_with("/config") {
        return None;
    }
    let rest = trimmed[7..].trim();
    let mut parts = rest.split_whitespace();
    let action = parts.next()?;
    let key = parts.next();
    let val = parts.next();
    Some((action, key, val))
}

#[derive(Debug, Clone, Copy)]
enum ToolCommand {
    RunNext,
    RunEntry(usize),
    SkipNext,
    SkipEntry(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RightPanelTab {
    #[default]
    ToolLogs,
    MarketData,
}

impl RightPanelTab {
    pub fn next(self) -> Self {
        match self {
            RightPanelTab::ToolLogs => RightPanelTab::MarketData,
            RightPanelTab::MarketData => RightPanelTab::ToolLogs,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            RightPanelTab::ToolLogs => "Tool Logs",
            RightPanelTab::MarketData => "Market Data",
        }
    }
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
    pub active_tab: RightPanelTab,
    pub market_context: MarketContext,
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
            active_tab: RightPanelTab::default(),
            market_context: MarketContext::default(),
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
            self.tool_scroll = 0;
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
    use tempfile::tempdir;

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

    #[allow(clippy::field_reassign_with_default)]
    #[test]
    fn build_system_prompt_mentions_write_policy() {
        let mut cfg = AppConfig::default();
        cfg.allow_tool_writes = false;
        let prompt = App::build_system_prompt(&cfg);
        assert!(
            prompt.contains("READ-ONLY"),
            "prompt should mention read-only mode:\n{prompt}"
        );

        cfg.allow_tool_writes = true;
        let prompt = App::build_system_prompt(&cfg);
        assert!(
            prompt.contains("ENABLED"),
            "prompt should mention write access:\n{prompt}"
        );
    }

    #[allow(clippy::field_reassign_with_default)]
    #[test]
    fn build_lua_tool_reflects_config() {
        let mut cfg = AppConfig::default();
        cfg.allow_tool_writes = false;
        let tool = App::build_lua_tool(&cfg);
        assert_eq!(tool.name, LLM_LUA_TOOL_NAME);
        assert!(
            tool.description.contains("read-only inspection"),
            "description should warn about read-only mode"
        );
        assert_eq!(tool.parameters["required"], serde_json::json!(["source"]));

        cfg.allow_tool_writes = true;
        let tool = App::build_lua_tool(&cfg);
        assert!(
            !tool.description.contains("read-only inspection"),
            "write-enabled configs should not append read-only warning"
        );
    }

    #[test]
    fn parse_lua_command_handles_whitespace() {
        assert_eq!(parse_lua_command("   /lua   return 1"), Some(LuaAction::Run("return 1")));
        assert_eq!(parse_lua_command("/lua"), Some(LuaAction::Run("")));
        assert_eq!(parse_lua_command("lua return 1"), None);
    }

    #[allow(clippy::field_reassign_with_default)]
    #[test]
    fn app_state_append_resets_scroll() {
        let mut state = AppState::default();
        state.chat_scroll = 7;
        let original = state.messages[0].content.clone();
        state.append_to_message(0, " extra");
        assert!(state.messages[0].content.ends_with(" extra"));
        assert_ne!(state.messages[0].content, original);
        assert_eq!(state.chat_scroll, 0);
    }

    #[test]
    fn app_state_updates_tool_log_entries() {
        let mut state = AppState::default();
        state
            .tool_logs
            .push(ToolLogEntry::new(42, "demo", "pending detail"));
        state.update_tool_log(42, ToolStatus::Success, "done");
        assert_eq!(state.tool_logs[0].status, ToolStatus::Success);
        assert_eq!(state.tool_logs[0].detail, "done");
    }

    #[test]
    fn input_state_handles_utf8_navigation() {
        let mut input = InputState::default();
        input.insert_char('你');
        input.insert_char('好');
        assert_eq!(input.buffer(), "你好");
        input.move_left();
        input.backspace();
        assert_eq!(input.buffer(), "好");
        input.move_to_start();
        input.insert_char('!');
        assert_eq!(input.buffer(), "!好");
        input.move_to_end();
        input.delete_char();
        assert_eq!(input.buffer(), "!好");
        assert!(input.cursor_display_offset() > 0);
    }



    #[test]
    fn adjust_chat_scroll_moves_up_and_down() {
        let mut offset = 0;
        adjust_chat_scroll(&mut offset, -3); // scroll up
        assert_eq!(offset, 3);
        adjust_chat_scroll(&mut offset, 2); // move towards bottom
        assert_eq!(offset, 1);
        adjust_chat_scroll(&mut offset, 10); // clamp at 0
        assert_eq!(offset, 0);
    }

    #[test]
    fn push_message_with_index_tracks_position() {
        let mut state = AppState::default();
        let idx = state.push_message_with_index(Message::new(Role::User, "next"));
        assert_eq!(idx, 1, "default state starts with welcome message");
        assert_eq!(state.messages[idx].content, "next");
    }

    #[allow(clippy::field_reassign_with_default)]
    #[test]
    fn remove_message_updates_scroll() {
        let mut state = AppState::default();
        state.chat_scroll = 5;
        let original_len = state.messages.len();
        state.remove_message(0);
        assert_eq!(state.messages.len(), original_len - 1);
        assert_eq!(state.chat_scroll, 0);
    }

    #[test]
    fn append_tool_call_appends_invocation() {
        let mut state = AppState::default();
        let idx = state.push_message_with_index(Message::new(Role::Assistant, "running tool"));
        let invocation =
            ToolInvocation::from_parts("demo", serde_json::json!({"value": 1}), Some("abc".into()));
        state.append_tool_call(idx, invocation.clone());
        assert_eq!(state.messages[idx].tool_calls.len(), 1);
        assert_eq!(state.messages[idx].tool_calls[0].name, invocation.name);
    }

    #[test]
    fn message_is_empty_checks_bounds() {
        let mut state = AppState::default();
        let idx = state.push_message_with_index(Message::new(Role::Assistant, ""));
        assert!(state.message_is_empty(idx));
        assert!(state.message_is_empty(usize::MAX));
    }

    #[test]
    fn render_tool_invocation_includes_metadata() {
        let invocation = ToolInvocation::from_parts(
            "lua_run_script",
            serde_json::json!({"source":"return 1"}),
            Some("call_1".into()),
        );
        let message = render_tool_invocation(invocation);
        assert!(message.content.contains("call_id: call_1"));
        assert!(message.content.contains("lua_run_script"));
    }

    #[test]
    fn stream_robustness_handles_partial_chunks() {
        let mut state = AppState::default();
        let idx = state.push_message_with_index(Message::new(Role::Assistant, ""));
        let (tx, rx) = mpsc::unbounded_channel();
        let (res_tx, res_rx) = std_mpsc::channel();
        let runtime = Runtime::new().unwrap();
        let handle = runtime.handle().clone();

        let mut app = App {
            config: AppConfig::default(),
            macros: MacroConfig::default(),
            state,
            llm: Arc::new(StubClient::new()),
            runtime,
            lua: LuaExecutor::new(".", false, handle).unwrap(),
            session: SessionRecorder::new(tempdir().unwrap().path(), false).unwrap(),
            should_quit: false,
            next_tool_id: 0,
            active_stream: Some(ActiveStream {
                receiver: rx,
                result_rx: res_rx,
                message_index: idx,
            }),
            pending_lua_tools: Vec::new(),
        };

        // Send chunks
        tx.send(StreamEvent::Delta("Hello".into())).unwrap();
        tx.send(StreamEvent::Delta(" World".into())).unwrap();
        
        app.poll_active_stream();
        assert_eq!(app.state.messages[idx].content, "Hello World");

        // Close stream successfully
        drop(tx);
        res_tx.send(Ok(())).unwrap();
        
        app.poll_active_stream();
        assert!(app.active_stream.is_none());
        assert_eq!(app.state.messages[idx].content, "Hello World");
    }

    #[test]
    fn multi_tool_queuing_works() {
        let mut state = AppState::default();
        let idx = state.push_message_with_index(Message::new(Role::Assistant, ""));
        let (tx, rx) = mpsc::unbounded_channel();
        let (res_tx, res_rx) = std_mpsc::channel();

        // Config with writes enabled to trigger queuing
        let mut config = AppConfig::default();
        config.allow_tool_writes = true;

        let runtime = Runtime::new().unwrap();
        let handle = runtime.handle().clone();

        let mut app = App {
            config,
            macros: MacroConfig::default(),
            state,
            llm: Arc::new(StubClient::new()),
            runtime,
            lua: LuaExecutor::new(".", false, handle).unwrap(),
            session: SessionRecorder::new(tempdir().unwrap().path(), false).unwrap(),
            should_quit: false,
            next_tool_id: 0,
            active_stream: Some(ActiveStream {
                receiver: rx,
                result_rx: res_rx,
                message_index: idx,
            }),
            pending_lua_tools: Vec::new(),
        };

        // Simulate receiving two tool calls
        let call1 = ToolInvocation::from_parts("lua_run_script", serde_json::json!({"source": "print(1)"}), Some("id1".into()));
        let call2 = ToolInvocation::from_parts("lua_run_script", serde_json::json!({"source": "print(2)"}), Some("id2".into()));

        tx.send(StreamEvent::ToolCall(call1)).unwrap();
        tx.send(StreamEvent::ToolCall(call2)).unwrap();
        
        app.poll_active_stream();
        
        // Check that both are queued
        assert_eq!(app.pending_lua_tools.len(), 2);
        assert_eq!(app.pending_lua_tools[0].script, "print(1)");
        assert_eq!(app.pending_lua_tools[1].script, "print(2)");
        
        // Check that tool log entries were created
        assert_eq!(app.state.tool_logs.len(), 2);
    }
}
