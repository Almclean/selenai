# 🚀 Announcing SelenAI: The Safe, Rust-Powered AI Terminal Agent

I'm excited to release the MVP of **SelenAI**, a terminal-based AI pair programmer built in Rust! 🦀

Unlike other agents that are black boxes, SelenAI runs in your terminal with full observability and safety controls.

**Key Features:**
✅ **Sandboxed Lua Toolchain**: The agent writes Lua scripts to inspect/edit your code. You see exactly what it's doing.
✅ **Safe Patching**: Edits are applied via verified unified diffs. No more broken files.
✅ **Multi-Tool Pipeline**: Parallel tool execution for faster workflows.
✅ **Safety First**: Run in Read-Only mode or approve writes interactively.
✅ **TUI Interface**: Side-by-side Chat & Tool Logs (powered by `ratatui`).
✅ **Review Mode**: `/review` automatically diffs your workspace for instant context.

**Get Started:**
```bash
git clone https://github.com/your-username/selenai
cargo run --release
```

**Cookbook:** `docs/recipes.md`
**Roadmap:** `docs/roadmap.md`

Feedback welcome! Let's build the future of terminal AI together. #RustLang #AI #DevTools #OpenSource
