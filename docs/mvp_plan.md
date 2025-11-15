# SelenAI MVP Plan

## Goals
- Build a Rust-based LLM agent loop that can chat with the user in the terminal.
- Integrate a single tool: a Lua sandbox powered by `mlua` that executes auto-generated helper code.
- Present the conversation, tool traces, and input box via a Ratatui-powered TUI.
- Keep the surface area small but production-ready: clean boundaries, async-friendly, and testable.

Non-goals for the MVP: multi-agent orchestration, distributed execution, UI theming, or advanced prompt caching.

## High-Level Architecture
```
+-------------------------+        +-----------------+
|  Ratatui TUI (frontend) |<------>|  Agent runtime  |
+-------------------------+        +-----------------+
             ^                               |
             | user events/input             v
    render loop / ticks            +--------------------+
                                   |  LLM Client Layer  |
                                   +--------------------+
                                              |
                                   HTTP call to provider
                                              |
                                   +--------------------+
                                   | Lua Tool Executor  |
                                   +--------------------+
```
- **TUI** renders panes (conversation, tool logs, input) and sends user prompts to the agent runtime.
- **Agent runtime** keeps conversation state, decides whether to call the tool, and orchestrates async work.
- **LLM client layer** abstracts whichever provider is used (e.g., OpenAI, local server); returns either plain text or a `ToolInvocation`.
- **Lua tool executor** is the only tool; it receives generated Lua snippets, injects a safe API surface, executes, and returns structured output.

## Crate Layout (proposed)
```
src/
 ├── main.rs                // bootstrap + tokio runtime
 ├── app.rs                 // Agent runtime state machine
 ├── tui/
 │    ├── mod.rs            // layout + widgets
 │    ├── components.rs     // conversation/log panes
 │    └── input.rs          // line editor
 ├── llm/
 │    ├── mod.rs            // interface trait + models
 │    └── openai.rs         // concrete HTTP client (optional for MVP)
 ├── lua_tool/
 │    ├── mod.rs            // sandbox creation + execution
 │    └── builtins.rs       // safe host functions exposed to Lua
 └── types.rs               // shared structs (messages, tool payloads)
```

## Key Subsystems
### 1. Agent Runtime
- Async task that owns conversation history (`Vec<Message>`), tool traces, and UI events.
- State machine:
  1. Receive user prompt.
  2. Append to history and call LLM client.
  3. If model responds with `ToolInvocation`, run Lua tool and append result as assistant message.
  4. Otherwise stream/display text reply.
- Add cancelation + simple retry for flaky tool runs.

### 2. LLM Client Layer
- Trait `LlModel { async fn chat(&self, ctx: &Conversation) -> Result<AgentEvent> }`.
- `AgentEvent` variants: `AssistantText(String)` or `ToolCall(ToolInvocation)`.
- MVP can stub out real HTTP by returning scripted responses, enabling offline work until credentials are available.
- Target OpenAI Responses API first, but keep provider-specific code isolated behind the trait so future backends can be swapped in.
- Support streaming responses (incremental text chunks + tool calls) so the TUI can render partial completions while the model is still generating.
- Use `reqwest` + `serde_json` when hooking up to a provider; keep provider-specific code isolated.

### 3. Lua Tool Executor
- Uses `mlua::{Lua, Function, Value}` with `Lua::new_with` (or `Lua::unsafe_new_with`) configured for sandboxing.
- At startup register host functions:
  - `fs_read(path)`, `fs_write(path, contents)` (limited to workspace root, use canonicalization).
  - `http_request(method, url, body?, headers?)` wrapping `reqwest`.
  - `log(level, message)` for tool traces.
- Execution pipeline:
  1. Receive `ToolInvocation { code: String, args: Value }`.
  2. Validate code (length limit, forbid `os.execute`, etc.).
  3. Run inside separate Lua instance with timeout (use `tokio::time::timeout` + poll `mlua::Thread`).
  4. Collect structured result (JSON string or Lua table converted via `serde`).
- Return `ToolResult` objects that are appended to the conversation for the LLM.
- Default to read-only helpers for MVP; expose write-capable helpers only after the user explicitly opts in (CLI flag or runtime confirmation).
- Every host call should emit a structured log entry that gets persisted so sessions are auditable after the TUI exits.

### 4. Ratatui Interface
- Layout proposal:
  - Top: Conversation pane (scrollable, shows speaker labels).
  - Middle: Tool log pane (running list of tool calls, status, stdout/stderr).
  - Bottom: Input box + status line (model name, tokens).
- Event loop:
  - `crossterm` for key events; poll via `tokio::select!` with agent updates.
  - Maintain app state in `AppViewModel`; update on new messages or tool results.
- Provide minimal commands: Enter to send, `Ctrl+C` to exit, `Ctrl+L` to clear logs.
- Stream model output into the conversation pane as soon as chunks arrive to keep the UI feeling responsive.
- Flush tool logs (and optionally chat transcripts) to disk under `.selenai/logs/` so runs can be replayed post-session.

## Dependencies to Add
- `tokio` (full features for signals + timeouts)
- `ratatui` + `crossterm`
- `mlua` (with `lua54` feature)
- `reqwest` (with `json` + `stream` features), `serde`, `serde_json`
- `anyhow` or `eyre` for error handling
- `tracing` + `tracing-subscriber`
- For tests: `assert_cmd` or `insta` optional later

## Provider & Policy Decisions
- **First provider**: OpenAI Responses API (gpt-4o-mini by default) surfaced via the `LlModel` trait.
- **Streaming**: Required; both the client layer and TUI must handle incremental responses.
- **Filesystem writes**: Disabled by default; enable only when the user grants permission for the current session.
- **Tool log persistence**: Always on; logs are written alongside run metadata for later debugging.

## Implementation Milestones
1. **Scaffold crate**
   - Set edition, add dependencies, enable `tokio::main`.
   - Structure modules per layout above.
2. **TUI Skeleton**
   - Render static panes, wire input handling, log dummy events.
   - Provide `AppEvent` enum for user vs. agent updates.
3. **Conversation State + LLM stub**
   - Define `Message`, `Role`, `ToolInvocation`.
   - Implement fake LLM client that echoes or triggers canned tool calls so flow can be end-to-end without network.
4. **Lua Tool MVP**
   - Expose one safe builtin (`fs_read`) to prove plumbing.
   - Run simple script (e.g., read file and return snippet) with timeout + error propagation.
5. **Wire Agent Loop**
   - Connect TUI input → agent → streaming LLM client → Lua tool → display results.
   - Show tool traces in dedicated pane and flush them to persisted logs.
6. **Real LLM Client (optional once credentials exist)**
   - Implement OpenAI-compatible client using the Responses streaming endpoint.
   - Inject API key via env var; add simple rate limiting + retries.
7. **Hardening**
 - Expand Lua sandbox (write, HTTP).
 - Add tests for host functions and guardrails.
 - Persist conversation transcripts and tool logs to disk (e.g., `.selenai/logs/`) alongside metadata describing whether writes were enabled.

## Near-Term Next Steps
1. Land the LLM abstraction (trait + request/response + streaming events) and move the stub client onto it.
2. Scaffold the OpenAI client (env/config plumbing, streaming handler) and hook up background tasks that forward chunks into the TUI.
3. Write out tool logs + transcripts to disk alongside metadata about gated write permissions.
4. Add configuration knobs (env vars/CLI) to choose provider, turn on write access, and point at log directories.

## Testing Strategy
- Unit tests for Lua builtins (fs access guarded, http stubbed).
- Integration test that runs agent loop in headless mode with stub UI + stub LLM to ensure tool call path works.
- Property tests for path sanitization.
- Manual TUI testing for UX.

## Open Questions / Follow-ups
- How should the write-access gate manifest in the TUI (modal prompt vs. CLI flag)?
- What format do we want for persisted logs/transcripts so future tooling can parse them (JSONL vs. SQLite, etc.)?
- Do we need provider-pluggability before MVP, or is OpenAI-only acceptable until post-MVP?

This plan should make it straightforward to start implementing files in the order shown, landing a working TUI chat agent whose only tool execution surface is the Lua sandbox.
