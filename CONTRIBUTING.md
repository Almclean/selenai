# Contributing to SelenAI

Thanks for helping shape the terminal-first AI pair-programmer! This doc
describes how to get a dev environment running, how to work with the sandboxed
Lua helpers, and what we look for in pull requests.

## Prerequisites
- Rust toolchain (2024 edition). `rustup default stable` works fine.
- Optional: OpenAI API key for the live client (`OPENAI_API_KEY` in your env or
  `.env`). The stub client works offline.
- A POSIX-friendly shell (macOS/Linux/WSL). Windows users can build inside WSL.

## Local setup
1. Fork + clone the repo.
2. Copy `selenai.toml` (or create a new file) and pick your provider:
   ```toml
   provider = "stub"     # offline echo client
   model_id = "gpt-4o-mini"
   streaming = true
   allow_tool_writes = false
   log_dir = ".selenai/logs"
   ```
3. (Optional) create a `.env` with `OPENAI_API_KEY=...` if you want to try the
   OpenAI client.
4. Run the TUI locally:
   ```bash
   cargo run
   ```
   Use `/lua` to run Lua snippets, `/tool run` to approve queued scripts, and
   `Ctrl+C`/`Esc` to exit. Every session writes JSONL transcripts + tool logs to
   the directory configured via `log_dir`.

## Testing
- `cargo test` – runs unit tests for the app state, Lua sandbox, OpenAI client,
  session recorder, etc. Please ensure it passes.
- When touching the Lua sandbox, consider adding targeted tests in
  `src/lua_tool/mod.rs` (there are helpers/fixtures already).
- For docs-only changes, tests aren’t required but appreciated if code touches
  accompany them.

## Coding guidelines
- Follow `rustfmt` defaults. CI will reject obviously unformatted code.
- Prefer small, focused PRs; describe the plan and reasoning in the PR body.
- Gate risky behaviors:
  - Keep writes disabled unless `allow_tool_writes = true`.
  - When adding new Lua host functions, document the safety story and add tests.
- Every new feature should mention how it fits into the MVP roadmap (see
  `docs/mvp_plan.md`) or the open roadmap issues.

## Docs & UX
- Update `README.md` / `docs/*.md` when introducing new config knobs, CLI
  commands, or UI behavior.
- If your change affects session logs, describe the new structure and consider
  bumping the metadata version in `src/session.rs`.
- Screenshots/gifs are welcome in PR descriptions but keep the repo ASCII-only.

## Pull requests
1. Open an issue (or comment on an existing one) if you’re planning a large
   feature so we can discuss scope.
2. Create a feature branch, commit with descriptive messages.
3. Run `cargo test`.
4. Submit the PR and link the relevant issue. Fill in the checklist (tests run,
   docs updated, etc.).

We’ll review with a focus on safety (sandbox boundaries), UX clarity, and test
coverage. Thanks for building with us!
