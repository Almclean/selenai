# Runtime Configuration

SelenAI reads configuration from `selenai.toml` in the project root (override with the
`SELENAI_CONFIG` environment variable). The file uses TOML syntax and lets you control the
LLM provider, model identifier, streaming, and whether Lua helpers may write to disk.

```toml
# LLM backend: "stub" for offline development, "openai" for the real API.
provider = "stub"

# Default model identifier. Applied to providers that accept model choices.
model_id = "gpt-4o-mini"

# Toggle streaming completions when the provider supports it.
streaming = true

# Keep false to run tools in read-only mode; set true to allow gated writes later.
allow_tool_writes = false

# Directory (relative to the workspace unless absolute) where chat transcripts and
# tool logs should be persisted after each run.
log_dir = ".selenai/logs"
```

Any field left blank falls back to safe defaults. When `provider = "openai"`, set `OPENAI_API_KEY`
either in your shell or by creating a `.env` file (automatically loaded on startup).

SelenAI writes a full transcript and tool log to the directory referenced by `log_dir`
every time you exit the TUI. Paths are resolved relative to the workspace unless you
provide an absolute value, and each session gets its own timestamped subdirectory with
metadata describing whether Lua writes were permitted.
