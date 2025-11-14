## Lua tool-call implementation plan

Guiding principles come from Cloudflare's [Code Mode blog](https://blog.cloudflare.com/code-mode/) and Anthropic's [Code Execution with MCP](https://www.anthropic.com/engineering/code-execution-with-mcp): the LLM must be explicitly told when and how it can run code, we must gate execution through a tool-calling protocol, and every run should be observable and reversible by the user.

### Tasks

1. **Baseline + requirements**
   - [x] Audit the existing `/lua` command, `LuaExecutor`, and `ToolLogEntry` flow to capture how scripts are executed today.
   - [x] Re-read the two guidance posts and distill concrete requirements (e.g., system prompt wording, confirmation expectations, safe defaults).

2. **System prompt + schema**
   - [x] Inject a system message into every `ChatRequest` that describes the Lua sandbox, when it is appropriate to use it, and safety rules inspired by the blogs.
   - [x] Define a structured tool schema (e.g., `lua_run_script` with `source` + optional description) and ensure the OpenAI client advertises it when streaming/unary calls are issued.

3. **Tool-call handling**
   - [x] Extend `ChatResponse::ToolCall` handling so that when the LLM requests `lua_run_script`, we enqueue that script for execution (with optional user confirmation depending on `allow_tool_writes`) and surface progress via the tool log panel.
   - [x] Propagate tool-call events during streaming so partial tool arguments accumulate correctly before execution kicks off.

4. **Execution + UX**
   - [x] Route tool-call executions through the existing `LuaExecutor`, ensuring writes respect `allow_tool_writes`, capture stdout/stderr/logs, and present the results back in chat.
   - [x] Consider UX touches inspired by the blogs: e.g., show the Lua script before running, let the user cancel, and stamp runs with clear success/error indicators.

5. **Validation**
   - [ ] Add targeted unit/integration tests where feasible (e.g., verifying the system prompt is prepended, tool schema is serialized, and tool-call events trigger Lua execution in mock mode).
   - [ ] Manually verify end-to-end by prompting the OpenAI backend to request a Lua run, watching the stream, and confirming the output shows up in both chat and the tool log.

We will check off each box as we implement the corresponding work.

### Task 1 notes

- `App::invoke_lua` (src/app.rs) is the only path that runs Lua right now. User-entered `/lua <code>` scripts get logged via `ToolLogEntry` and the output is echoed back as a `Role::Tool` message, keeping stdout/stderr/log buffers separate.
- `LuaExecutor` (src/lua_tool/mod.rs) exposes `rust.read_file`, `list_dir`, `write_file` (gated by `allow_tool_writes`), `http_request`, `log`, `eprint`, and nested `mcp` helpers. The executor already scrubs dangerous globals and funnels filesystem operations through `resolve_safe_path`, so tool calls can reuse it safely.
- From Cloudflare's Code Mode post: encourage the model to use code execution for non-trivial edits, verify changes before editing files, and keep a tight loop of “plan → run → inspect → apply”. The LLM should explain why it needs Lua, show the script, and prefer diff-based edits.
- From Anthropic's MCP write-up: every tool call should be explicit with structured arguments, the assistant should be transparent about tool usage, and runs should be cancellable/retryable with clear status. We should mention the sandbox limitations, read/write policy, and expectation that the model double-checks results before concluding.
- OpenAI API reference: https://platform.openai.com/docs/api-reference/introduction – use this to confirm payload formats for tool definitions, streaming deltas, and finish reasons as we wire up Lua tool calls.

### Task 2 notes

- `App::build_system_prompt` now injects expectations around planning, verifying, transparency, and the Lua sandbox’s read/write capabilities (mirroring Code Mode + MCP guidelines) before every request.
- `App::build_lua_tool` constructs a `lua_run_script` tool definition with structured parameters (`source`, `reason`). The tool list and system prompt are attached to each `ChatRequest`, and `OpenAiClient::build_payload` serializes them into the REST payload for both streaming and unary calls.

### Task 5 notes

- Added unit tests that assert `ChatRequest` retains its system prompt/tool metadata and that the OpenAI payload includes the synthetic system message plus tool definitions. Will extend with execution-path tests once the Lua tool-call wiring lands.
- Added unit tests for Lua tool argument parsing/truncation helpers so malformed tool calls fail fast without touching the sandbox.
- Added unit tests for the `/tool` command parser so approving/canceling queued runs remains predictable as the UX evolves.

### Task 3 notes

- `ChatResponse::ToolCall` (both unary and streaming) now routes through `App::handle_tool_call`, which recognizes `lua_run_script`, shows a summary of the reason/script, logs the request, and executes it via `LuaExecutor`.
- The `run_lua_script` helper reuses the existing tool log + message flow so streamed tool calls show up immediately in the tool panel and chat transcript. Unknown tools fall back to the previous textual rendering for visibility.
- When `allow_tool_writes` is disabled we run scripts immediately (read-only), but with writes enabled we now queue requests for manual approval to keep risky mutations in check.
- Tool-call IDs from OpenAI are now propagated through execution so the tool outputs we send back to the LLM are attached to the correct `tool_call_id`, which also fixes the 400 errors when the conversation history contains tool responses.

### Task 4 notes

- Tool calls now reuse `run_lua_script`, so stdout/stderr/log output is identical to manual `/lua` runs, and the tool log shows `Pending` → `ok/error` transitions with the script content captured before execution.
- When write helpers are enabled we now queue each `lua_run_script` request and require the user to run `/tool run` (with optional entry id) to approve or `/tool skip` to cancel, ensuring the LLM can’t mutate files without an explicit confirmation step. Read-only setups continue to auto-run immediately.
- Each queued request surfaces its reason/script in chat plus a tool-log entry, mirroring the transparency guidance from the Code Mode and MCP blogs.
