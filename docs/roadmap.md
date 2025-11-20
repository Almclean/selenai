# SelenAI Roadmap

The MVP is stable; this plan prioritizes safer write flows, richer toolchains, and better observability for the code-mode loop.

## Near-Term Polish
- Streaming robustness: tighten `src/app.rs` state transitions; add tests for mid-stream tool emissions, cancellations, and partial outputs.
- Session hygiene: predictable log naming/rotation under `.selenai/logs`; optional redaction hook for secrets before writing transcripts.
- Lua sandbox hardening: stricter path guards (symlink detection, max file size) plus denial-case tests in `src/lua_tool/`.

## Capability Expansions
- Multi-tool pipeline: support multiple approved tool runs per turn with a queued/parallel view in `tui/`, enabling batch reads followed by a single write.
- Safe patch helper: add `rust.patch_file(path, diff)` to apply unified diffs with previews in the tool log and explicit approvals.
- MCP/server helpers: ship a first-party MCP server exposing repo-aware tools (ripgrep proxy, rustfmt formatter, git status) callable via `/tool load`.

## Experience & Ergonomics
- PR review mode: command to load a commit/PR diff, have the agent surface findings, and render inline annotations in the tool pane.
- Config presets: provide `selenai.toml` profiles (offline stub, OpenAI prod, “safe write”) plus an interactive `/config show|set` toggle in the TUI.
- User macros: support command macros stored in `~/.config/selenai/macros.toml`, surfaced in the hint line for quick reuse.

## Observability & Quality
- Tracing: optional `tracing` spans around LLM calls/tool runs with per-turn latency stats; enable via `SELENAI_TRACE=pretty`.
- Coverage guardrails: “changed-files” test runner and CI check that ensures tests touch modified modules using `cargo tarpaulin` or `cargo llvm-cov`.

## Docs & Onboarding
- Guided tour: first-run flow demonstrating `/lua`, `/tool run`, streaming, and approvals inside the TUI.
- Cookbook: `docs/recipes.md` with snippets for batch file reads, patch application, HTTP requests, and config tips.

## Suggested Sequence
1) Safe patch helper + streaming hardening/tests to improve write-path trust.
2) Session hygiene + sandbox guardrails for reliability.
3) Multi-tool pipeline and MCP helper server to unlock richer workflows.
4) PR review mode and config presets for daily usability.
5) Tracing and coverage guardrails to keep regressions visible.
6) Guided tour and cookbook to level up new users quickly.
