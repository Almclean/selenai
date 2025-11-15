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

[openai]
# Optional overrides; env vars OPENAI_API_KEY / OPENAI_BASE_URL etc. are also honored.
# OPENAI_API_KEY now comes from .env / the shell environment.
# base_url = "https://api.openai.com/v1"
# organization = ""
# project = ""
```

Any field left blank falls back to safe defaults. When `provider = "openai"`, set `OPENAI_API_KEY`
either in your shell or by creating a `.env` file (automatically loaded on startup).
