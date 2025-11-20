# Repository Guidelines

## Project Structure & Module Organization
- Rust 2024 workspace with sources under `src/`: `main.rs` (entry), `app.rs` (state + event loop), `tui/` (Ratatui widgets), `llm/` (OpenAI + stub clients), `lua_tool/` (sandboxed helpers), `session.rs` (logging), and `types.rs`.
- Documentation lives in `docs/` (`config.md`, `mvp_plan.md`, `lua_tool_plan.md`); tweak defaults in `selenai.toml`.
- Runtime logs land in `.selenai/logs` by default; keep large artifacts out of version control.

## Build, Test, and Development Commands
- `cargo run` / `cargo run --release` – start the TUI; pick models via `selenai.toml` or `SELENAI_CONFIG`.
- `cargo test` – run unit/integration tests (covers config parsing, chat flow, and Lua sandbox).
- `cargo fmt` and `cargo clippy --all-targets --all-features` – required lint/format pass; CI enforces rustfmt defaults.
- `cargo tarpaulin` – quick coverage sweep; mirrors CI’s coverage step.
- Example env toggles: `RUST_LOG=debug cargo run` for verbose logs; `SELENAI_DEBUG_OPENAI=1` to inspect REST payloads.

## Coding Style & Naming Conventions
- Follow rustfmt defaults; keep modules small and focused, returning `anyhow::Result` for fallible paths.
- Use snake_case for modules/functions, CamelCase for types, and keep user-facing strings concise for the TUI.
- Prefer clear separation between UI (`tui/`), runtime state (`app.rs`), and tooling (`lua_tool/`); add comments sparingly when behavior is non-obvious.

## Testing Guidelines
- Tests live alongside code in `#[cfg(test)] mod tests` blocks (see `src/lua_tool/mod.rs`, `src/app.rs`, etc.).
- Name tests after behavior (`handles_stream_cancel`, `rejects_outside_path`) and cover code paths that touch sandbox safety.
- Run `cargo test` before pushing; use `cargo tarpaulin` when coverage might drop.

## Commit & Pull Request Guidelines
- Write concise, present-tense commits (e.g., “Add tool approval log”) and keep scopes small.
- Open a feature branch, run `cargo fmt`, `cargo clippy`, and `cargo test`, then push.
- PRs should link relevant issues, summarize the change and rationale, note risk areas (especially sandbox boundaries), and mention doc/UX updates. Screenshots/gifs are welcome in PR descriptions; keep checked-in files ASCII-only.

## Security & Configuration Tips
- Default to `allow_tool_writes = false`; when enabling, approve queued `/tool run` requests explicitly to keep mutations auditable.
- Keep secrets (e.g., `OPENAI_API_KEY`) in `.env` or environment variables; do not commit them. Use `SELENAI_CONFIG` to point at per-project configs without changing the repo copy.
- Session transcripts and tool logs persist under `.selenai/logs`; avoid storing sensitive content there when sharing.
