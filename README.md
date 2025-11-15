# SelenAI

SelenAI is a terminal-first pair-programming environment that combines a Ratatui
interface, a pluggable LLM client, and a sandboxed Lua helper VM. You chat with
the assistant, watch every tool call that it performs, and optionally approve
write-capable scripts before they touch your workspace.

## Highlights
- **Transparent agent loop** – the chat pane, tool log, and input box keep you
  up to date on what the model is planning, running, and returning.
- **Sandboxed Lua helpers** – the only tool exposed to the model runs inside a
  locked-down Lua VM with explicit `rust.*` host functions for reading files,
  listing directories, making HTTP requests, and (optionally) writing paths
  inside the repo.
- **Plain-Lua ergonomics** – familiar `io.*` handles and `fs.*` helpers are
  pre-injected so the LLM can write idiomatic Lua and let the sandbox reroute
  operations safely.
- **Provider-agnostic LLM layer** – swap between the offline stub client and
  OpenAI by editing `selenai.toml` (or exporting `SELENAI_CONFIG`).
- **Plan-first design ethos** – the default system prompt mirrors the guidance
  from Cloudflare’s Code Mode + Anthropic’s MCP posts: plan → run tools →
  inspect → edit, and always explain the scripts you are about to execute.
- ✨ **Persisted session logs** – every run leaves behind chat transcripts and tool
  logs under `.selenai/logs` (configurable) so you can review or diff later.

![SelenAI demo](https://private-user-images.githubusercontent.com/37902/514817922-d268f8ba-abb8-49c0-a99c-cf5e0455b2c0.gif?jwt=eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.eyJpc3MiOiJnaXRodWIuY29tIiwiYXVkIjoicmF3LmdpdGh1YnVzZXJjb250ZW50LmNvbSIsImtleSI6ImtleTUiLCJleHAiOjE3NjMyNTA4MDUsIm5iZiI6MTc2MzI1MDUwNSwicGF0aCI6Ii8zNzkwMi81MTQ4MTc5MjItZDI2OGY4YmEtYWJiOC00OWMwLWE5OWMtY2Y1ZTA0NTViMmMwLmdpZj9YLUFtei1BbGdvcml0aG09QVdTNC1ITUFDLVNIQTI1NiZYLUFtei1DcmVkZW50aWFsPUFLSUFWQ09EWUxTQTUzUFFLNFpBJTJGMjAyNTExMTUlMkZ1cy1lYXN0LTElMkZzMyUyRmF3czRfcmVxdWVzdCZYLUFtei1EYXRlPTIwMjUxMTE1VDIzNDgyNVomWC1BbXotRXhwaXJlcz0zMDAmWC1BbXotU2lnbmF0dXJlPTZmZjAwNjg1YTczYTZjZDA5ZjNjNDk4NDgwZDhmZGI2NjYwMTM4YzIyYTk4OWYwNmExMmM0ZDAxNjlkYTkwMGMmWC1BbXotU2lnbmVkSGVhZGVycz1ob3N0In0.9fYW0XpZb85U5qU5o7RrUDjdKqFA5SncDyVLSgoSHGM)

## Table of Contents
- [Highlights](#highlights)
- [Quick Start](#quick-start)
- [Runtime Configuration](#runtime-configuration)
- [Working in the TUI](#working-in-the-tui)
- [Lua Sandbox API](#lua-sandbox-api)
- [Architecture Overview](#architecture-overview)
- [Engineering Principles](#engineering-principles)
- [Development Workflow](#development-workflow)
- [Troubleshooting](#troubleshooting)
- [Contributing](#contributing)
- [Docs](docs/)

---

## Code Coverage

[![codecov](https://codecov.io/github/Almclean/selenai/graph/badge.svg?token=3S8BPSQJ7A)](https://codecov.io/github/Almclean/selenai)

## Quick Start
1. **Install prerequisites**
   - Rust toolchain (edition 2024; `rustup default stable` is sufficient).
   - Optional: OpenAI API key if you want to use a real model.
2. **Clone and inspect**
   ```bash
   git clone <this repo>
   cd selenai
   ```
3. **Pick a configuration**
   - Copy `selenai.toml` (or create a new file) and set the provider + model.
   - For offline hacking, keep `provider = "stub"` to use the built-in echo
     client.
   - Point `SELENAI_CONFIG` at another TOML file if you need per-project
     overrides.
4. **Run the TUI**
   ```bash
   cargo run
   ```
   You should see the ASCII banner plus an input prompt. Type messages, press
   `Enter`, and use the keys described below to navigate.

> Tip: Set `allow_tool_writes = true` only if you are comfortable approving
> filesystem edits during the session—writes stay gated behind `/tool run`.

---

## Runtime Configuration
SelenAI reads configuration from `selenai.toml` (or the path stored in
`SELENAI_CONFIG`). Every field has a safe default, so you can omit keys you
don’t care about.

```toml
# selenai.toml
provider = "openai"      # or "stub" for offline usage
model_id = "gpt-4o-mini" # passed through to the provider
streaming = true         # request incremental deltas when supported
allow_tool_writes = false
log_dir = ".selenai/logs" # per-session transcripts + tool logs

[openai]
# API keys now live in OPENAI_API_KEY (set it in .env or export it before running).
# base_url = "https://api.openai.com/v1"
# organization = ""
# project = ""
```

SelenAI automatically loads a `.env` file from the workspace root (if present) before
reading configuration, so you can keep `OPENAI_API_KEY` and friends out of version control.

Environment variables:
- `SELENAI_CONFIG` – path to the config file (defaults to `./selenai.toml`).
- `OPENAI_API_KEY`, `OPENAI_BASE_URL`, `OPENAI_ORG`, `OPENAI_PROJECT` – used
  when `provider = "openai"` and not overridden in the file.
- `SELENAI_DEBUG_OPENAI=1` – dump REST payloads to stderr for debugging.
- `SELENAI_LOG_DIR` is unnecessary now that `log_dir` lives in the config, but
  you can still point `log_dir` at an absolute path if you want logs elsewhere.

See `docs/config.md` for the full reference.

---

## Working in the TUI
SelenAI renders three stacked panes: **Conversation** (top), **Tool activity** (middle), and
**Input** (bottom). The conversation pane accounts for wrapped lines so even long responses stay
scrollable; borders disappear automatically in copy-friendly mode.

### Navigation & habits
- `Tab` / `Shift+Tab` – switch between **Conversation**, **Tool activity**, and
  **Input** panes.
- `Up/Down/PageUp/PageDown` – scroll the focused pane; SelenAI keeps the chat
  pinned to the bottom unless you scroll away.
- `Ctrl+C` or `Esc` – exit; `Ctrl+L` clears tool logs; `Ctrl+U` clears the input
  buffer; `Ctrl+B` toggles copy-friendly mode (hides borders).
- The hint line above the input reminds you which pane currently has focus.

### Chatting vs. running scripts
- Plain text prompts go straight to the configured LLM.
- `/lua <script>` executes a Lua snippet immediately through the sandbox.
- `/tool run [id]` and `/tool skip [id]` approve or cancel queued tool runs when
  `allow_tool_writes = true`. Without an `id`, the commands target the oldest
  pending entry.
- Paste support is built in—just paste text while the input pane is focused.

### Streaming workflow
When `streaming = true` and the provider supports it, assistant responses appear
incrementally. Tool calls triggered mid-stream show up in both the chat pane and
tool log with the Lua source, reason, and current status.

### Session logs
Exiting the app writes a JSONL transcript of the chat plus the tool log to the
directory configured via `log_dir` (default `.selenai/logs`). Each run gets a
timestamped subdirectory that also records whether Lua writes were enabled, so
you can review exactly what happened later.

---

## Lua Sandbox API
The `lua_run_script` tool exposes a deliberately small surface area to keep
script execution auditable and reproducible:

| Helper | Description |
| ------ | ----------- |
| `io.open`, `io.read`, `io.write`, `io.lines` | Standard Lua-style file handles backed by the sandbox. Write modes still honor the `allow_tool_writes` gate and flush on `:close()`. |
| `fs.read`, `fs.write`, `fs.list` | Sugar wrappers over the `rust.*` helpers for quick one-off file or directory calls. |
| `rust.read_file(path)` | Read UTF-8 files under the repo root (path traversal is blocked). |
| `rust.list_dir(path)` | Return metadata about direct children of a directory. |
| `rust.write_file(path, contents)` | Write files inside the repo when `allow_tool_writes = true`; parents are created automatically. |
| `rust.http_request{ url, method?, headers?, body? }` | Synchronous HTTP helper via `reqwest::blocking::Client`. |
| `rust.log(message or {level?, message})` | Append entries to the tool log (rendered in TUI). |
| `rust.eprint{ message }` | Attach stderr-like notes to the tool output. |
| `rust.mcp.list_servers()` / `list_tools(server)` / `load_tool(server, tool)` | Explore helper files under `servers/`. |
| `print(...)` / `warn(...)` | Captured as stdout/stderr in the UI. |

Globals such as `os` and unrestricted `require` remain disabled; only the helpers
above (plus the sandboxed `require("rust")`) are exposed. Each run returns:

```
Return value: <stringified Lua value>
stdout:
  ...
stderr:
  ...
logs:
  [info] inspected Cargo.toml
```

When write helpers are enabled, every automatically requested tool run is queued
until you explicitly approve it via `/tool run` to keep the LLM honest about
mutating your workspace.

**Third-party Lua modules:** If you need additional pure-Lua libraries, vendor
them under the repository (e.g., `lua_libs/json.lua`) and load them with
`load(rust.read_file("lua_libs/json.lua"), "json", "t", {})()`. Global installs
or networked `require` calls remain disabled by design.

---

## Architecture Overview
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

Code layout:
- `src/main.rs` – bootstraps the runtime and hands control to `App`.
- `src/app.rs` – owns application state, handles key events, coordinates chat
  requests, tool approvals, streaming output, and Lua execution.
- `src/tui/` – Ratatui components (`render_chat`, tool pane, input box) and UI
  helpers such as copy-friendly mode + cursor placement.
- `src/llm/` – provider-agnostic types (`ChatRequest`, `ChatResponse`,
  `StreamEvent`) plus concrete clients (`openai.rs`, `StubClient`).
- `src/lua_tool/` – sandbox implementation, host function registration, and
  safety checks (`resolve_safe_path`, `ensure_single_component`).
- `src/types.rs` – shared `Message`, `Role`, and `ToolInvocation` structures.

The agent loop always:
1. Builds a system prompt describing expectations + tool affordances.
2. Appends your latest message and issues a chat request (streaming when
   possible).
3. Responds to either assistant text or a `ToolInvocation`. The only supported
   tool is `lua_run_script`, but unknown tools still get surfaced verbatim for
   transparency.
4. Logs stdout/stderr/log buffers and pushes the results back into the chat so
   the LLM can continue reasoning with real data rather than guesses.

---

## Engineering Principles
- **Plan before acting** – major edits should include a short plan in the chat
  pane before any file changes happen.
- **Prefer verified context** – the Lua tool should be the first resort for
  reading files, running quick calculations, or validating assumptions.
- **Read-only by default** – writes are opt-in (`allow_tool_writes`) and still
  require explicit approval to execute.
- **Human-in-the-loop transparency** – every tool request states why it is
  needed, shows the exact script, and emits structured logs so you can audit the
  run later.

---

## Development Workflow
- **Build / run:** `cargo run` (add `--release` for longer sessions).
- **Tests:** `cargo test` covers configuration parsing, tool request handling,
  and OpenAI payload generation.
- **Coverage:** `cargo tarpaulin` is available out of the box for quick line
  coverage sweeps (it takes a couple extra seconds to spin up the sandboxed
  Lua runtime but needs no additional setup).
- **CI:** pushes and PRs run `cargo fmt`, `cargo clippy`, `cargo test`, and
  `cargo tarpaulin` via `.github/workflows/ci.yml` so regressions are caught
  automatically.
- **Style:** Edition 2024 + `rustfmt` defaults. Favor small, focused modules and
  return `anyhow::Result` from fallible paths.
- **Debugging:** set `RUST_LOG` as needed; use `SELENAI_DEBUG_OPENAI=1` to print
  REST payloads when validating provider behavior.
- **Dependencies:** check `Cargo.toml` – Tokio (rt-multi-thread), Ratatui,
  Crossterm, mlua (`lua54`, `vendored`), Reqwest with `rustls`, Serde, and
  Unicode-width utilities.

When contributing, skim `docs/mvp_plan.md` and `docs/lua_tool_plan.md` for the
current roadmap and guardrails around tool execution UX.

---

## Troubleshooting
- **“OpenAI chat failed (401)”** – confirm `OPENAI_API_KEY` is set (for example via `.env`).
  Also check organization/project headers if your account requires them.
- **Lua helper says “write helpers are disabled”** – flip `allow_tool_writes` to
  `true` in your config *and* approve the run with `/tool run`.
- **The UI is blank or keyboard is stuck** – ensure the terminal supports
  `crossterm` (most POSIX terminals do). Exit via `Ctrl+C` to restore the
  cursor if you panic out of a run.
- **Streaming never starts** – confirm `streaming = true` and the provider
  supports it. The stub client ignores streaming requests and just returns a
  single chunk.

---

## Contributing
Issues and pull requests are welcome! See
[CONTRIBUTING.md](CONTRIBUTING.md) for local setup steps, testing tips, and
guidelines for adding Lua helpers or provider integrations.

## Additional Reading
- `docs/mvp_plan.md` – big-picture roadmap, architecture, and testing strategy.
- `docs/lua_tool_plan.md` – rationale behind the Lua tool UX and prompts.
- `docs/config.md` – expanded config documentation.

Questions or ideas? Drop them in chat while the app is running—the assistant is
primed to reason about this repo and aggressively use the Lua tool to stay in
sync with your code. Happy pairing!
