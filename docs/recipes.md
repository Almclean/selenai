# SelenAI Cookbook

This cookbook provides snippets and patterns for getting the most out of SelenAI's Lua toolchain and commands.

## 🔧 Lua Tool Patterns

The agent uses these patterns internally, but you can also run them manually via `/lua`.

### 1. Batch File Reading
Read multiple files in one go to save turns.

```lua
local files = {"src/main.rs", "Cargo.toml"}
local results = {}
for _, path in ipairs(files) do
    table.insert(results, "--- " .. path .. "\n" .. rust.read_file(path))
end
return table.concat(results, "\n")
```

### 2. Safe Patching
Apply a unified diff safely. The agent does this automatically, but here's the manual syntax:

```lua
local diff = """
---" src/main.rs
+++ src/main.rs
@@ -1,3 +1,3 @@
 fn main() {
-    println!(\"Hello\");
+    println!(\"Hello World\");
 }
"""
rust.patch_file("src/main.rs", diff)
```

### 3. HTTP Requests
Fetch external data (e.g., docs or an API) to inform your code.

```lua
local resp = rust.http_request({
    url = "https://api.github.com/repos/owner/repo/issues/1",
    headers = {["User-Agent"] = "SelenAI"}
})
return resp.body
```

### 4. Grep Search
Search for a pattern in the codebase.

```lua
-- Search for "TODO" in the "src" directory
return rust.search("TODO", "src").stdout
```

### 5. Git Status & Diff
Check what's changed before asking for a review.

```lua
local status = rust.git_status().stdout
if status ~= "" then
    return rust.run_command("git", {"diff"}).stdout
else
    return "Clean working tree."
end
```

---

## ⌨️ Commands

| Command | Description |
| :--- |
| `/lua <code>` | Run Lua code directly in the sandbox. |
| `/review [path]` | Load `git status` and `git diff` (optional `path`) into context. |
| `/config show` | Display current session configuration. |
| `/config set <key> <val>` | Update config (e.g., `allow_tool_writes true`). |
| `/tool run [id]` | Approve a pending tool execution. |
| `/tool skip [id]` | Cancel a pending tool execution. |

## ⚡ Macros
Define these in `~/.config/selenai/macros.toml`:

```toml
[macros]
# Type @test to run this
test = "/lua return rust.run_command('cargo', {'test'}).stdout"

# Type @check to run clippy
check = "/lua return rust.run_command('cargo', {'clippy'}).stdout"
```
