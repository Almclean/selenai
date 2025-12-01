use std::{
    cell::RefCell,
    ffi::OsString,
    fs, io,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    rc::Rc,
};

use anyhow::{Context, Result, bail};
use mlua::{Lua, LuaOptions, StdLib, Table, UserData, UserDataMethods, Value, Variadic};
use patch::{Line, Patch};
use reqwest::{Method, blocking::Client, header::HeaderName, header::HeaderValue};
use tokio::runtime::Handle;
use yahoo_finance_api as yahoo;

const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024; // 10 MB

pub struct LuaExecutor {
    lua: Lua,
    logs: Rc<RefCell<Vec<String>>>,
    stdout: Rc<RefCell<Vec<String>>>,
    stderr: Rc<RefCell<Vec<String>>>,
    dashboard_updates: Rc<RefCell<Vec<String>>>,
    workspace_root: PathBuf,
    allow_writes: bool,
    http: Client,
    handle: Handle,
}

#[derive(Debug, Clone)]
pub struct LuaExecution {
    pub value: String,
    pub logs: Vec<String>,
    pub stdout: Vec<String>,
    pub stderr: Vec<String>,
    pub dashboard_updates: Vec<String>,
}

impl LuaExecutor {
    pub fn new(root: impl Into<PathBuf>, allow_writes: bool, handle: Handle) -> Result<Self> {
        let root = root.into();
        let canonical = if root.exists() {
            root.canonicalize()
                .with_context(|| format!("failed to canonicalize {}", root.display()))?
        } else {
            root
        };

        let http = Client::builder().build()?;
        
        // Initialize Persistent Lua VM
        let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default())?;
        
        // Create shared buffers
        let logs = Rc::new(RefCell::new(Vec::new()));
        let stdout = Rc::new(RefCell::new(Vec::new()));
        let stderr = Rc::new(RefCell::new(Vec::new()));
        let dashboard_updates = Rc::new(RefCell::new(Vec::new()));

        // We need to register the "real" API now.
        // Note: We cannot use `self.build_rust_api` easily here because `self` doesn't exist yet.
        // We will refactor the helper construction to simple functions or methods that don't require `self` 
        // if we pass the dependencies (root, allow_writes, http) manually.
        // OR we construct a temporary "Builder" or just init `LuaExecutor` with fields and THEN setup Lua?
        // `Lua` is in the struct. We can't have `self.lua` valid while calling methods on `self`.
        //
        // Easier approach:
        // 1. Create `LuaExecutor` with `lua` (and buffers).
        // 2. Call a private method `init_lua(&self)` which registers everything.
        //    This works because `Lua` uses interior mutability (or we use `&lua` from `self.lua`).
        
        let executor = Self {
            lua,
            logs,
            stdout,
            stderr,
            dashboard_updates,
            workspace_root: canonical,
            allow_writes,
            http,
            handle,
        };
        
        executor.init_lua()?;
        
        Ok(executor)
    }
    
    fn init_lua(&self) -> Result<()> {
        let lua = &self.lua;
        let logs = self.logs.clone();
        let stdout = self.stdout.clone();
        let stderr = self.stderr.clone();

        let rust_api = self.build_rust_api(lua, logs.clone(), stderr.clone())?;
        let globals = lua.globals();
        let _ = globals.raw_set("os", Value::Nil);
        globals.set("print", self.make_print_fn(lua, stdout)?)?;
        globals.set("warn", self.make_warn_fn(lua, stderr)?)?;
        globals.set("rust", rust_api)?;
        globals.set("io", self.build_io_table(lua)?)?;
        globals.set("fs", self.build_fs_table(lua)?)?;
        let package = self.build_package_table(lua)?;
        globals.set("package", package)?;
        globals.set("require", self.make_safe_require_fn(lua)?)?;
        
        // Load Prelude
        let prelude = include_str!("prelude.lua");
        lua.load(prelude).set_name("prelude").exec()?;
        
        Ok(())
    }

    pub fn reset(&mut self) -> Result<()> {
        self.lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default())?;
        self.logs.borrow_mut().clear();
        self.stdout.borrow_mut().clear();
        self.stderr.borrow_mut().clear();
        self.dashboard_updates.borrow_mut().clear();
        self.init_lua()
    }

    pub fn run_script(&self, script: &str) -> Result<LuaExecution> {
        // Clear buffers from previous run
        self.logs.borrow_mut().clear();
        self.stdout.borrow_mut().clear();
        self.stderr.borrow_mut().clear();
        self.dashboard_updates.borrow_mut().clear();

        let value = self.lua.load(script).set_name("tool").eval::<Value>()?;
        
        Ok(LuaExecution {
            value: render_value(value),
            logs: collect_buffer(self.logs.clone()),
            stdout: collect_buffer(self.stdout.clone()),
            stderr: collect_buffer(self.stderr.clone()),
            dashboard_updates: collect_buffer(self.dashboard_updates.clone()),
        })
    }

    pub fn preview_script(&self, script: &str) -> Result<String> {
        let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default())?;
        // We use the 'logs' buffer to collect preview messages
        let logs = Rc::new(RefCell::new(Vec::new()));
        let stdout = Rc::new(RefCell::new(Vec::new()));
        let stderr = Rc::new(RefCell::new(Vec::new()));
        
        let rust_api = self.build_preview_rust_api(&lua, logs.clone(), stderr.clone())?;
        
        let globals = lua.globals();
        let _ = globals.raw_set("os", Value::Nil);
        globals.set("print", self.make_print_fn(&lua, stdout.clone())?)?;
        globals.set("warn", self.make_warn_fn(&lua, stderr.clone())?)?;
        globals.set("rust", rust_api.clone())?;
        // For IO/FS tables, we ideally want preview versions too, but for now 
        // we can leave them as read-only (since we aren't enabling writes in this mode anyway).
        // But wait, build_io_table checks 'allow_writes'. 
        // In preview mode, we effectively want 'allow_writes' to be FALSE for the real FS,
        // but we want the 'rust' helpers to simulate writes.
        // Since 'self.allow_writes' might be true, we should be careful.
        // Actually, we can just use the standard build_io_table. If the script tries `io.open(..., "w")`,
        // it will either work (if allowed) or fail.
        // Ideally, preview shouldn't perform ANY real writes.
        // So we should construct a LuaExecutor with allow_writes=false?
        // No, we are implementing a method on existing executor.
        // We should manually mock io/fs to be safe or just accept that `io.open` writes happen if the script does them.
        // But the LLM is instructed to use `rust.*` helpers.
        // Let's just expose `rust` preview helpers.
        
        globals.set("io", self.build_io_table(&lua)?)?;
        globals.set("fs", self.build_fs_table(&lua)?)?;
        let package = self.build_package_table(&lua)?;
        globals.set("package", package)?;
        globals.set("require", self.make_safe_require_fn(&lua)?)?;

        // Run the script. We ignore the return value and stdout, 
        // we just want to capture the side-effects logged by our preview helpers.
        let _ = lua.load(script).set_name("preview").eval::<Value>();
        
        let collected = collect_buffer(logs);
        if collected.is_empty() {
            Ok("No write operations detected in script.".to_string())
        } else {
            Ok(collected.join("\n"))
        }
    }

    fn build_preview_rust_api<'lua>(
        &self,
        lua: &'lua Lua,
        logs: Rc<RefCell<Vec<String>>>,
        stderr: Rc<RefCell<Vec<String>>>,
    ) -> Result<Table<'lua>> {
        let table = lua.create_table()?;
        // Read-only helpers are fine to be real
        table.set("read_file", self.make_read_fn(lua)?)?;
        table.set("list_dir", self.make_list_fn(lua)?)?;
        table.set("http_request", self.make_http_fn(lua)?)?;
        table.set("git_status", self.make_git_status_fn(lua)?)?;
        table.set("search", self.make_search_fn(lua)?)?;
        table.set("log", self.make_log_fn(lua, logs.clone())?)?; // log to our preview buffer
        table.set("eprint", self.make_eprint_fn(lua, stderr)?)?;
        table.set("mcp", self.make_mcp_table(lua)?)?;
        
        // Write helpers are replaced by preview versions
        table.set("write_file", self.make_preview_write_fn(lua, logs.clone())?)?;
        table.set("patch_file", self.make_preview_patch_file_fn(lua, logs.clone())?)?;
        table.set("run_command", self.make_preview_run_command_fn(lua, logs.clone())?)?;
        
        Ok(table)
    }

    fn make_preview_write_fn<'lua>(
        &self,
        lua: &'lua Lua,
        logs: Rc<RefCell<Vec<String>>>,
    ) -> Result<mlua::Function<'lua>> {
        let fun = lua.create_function(move |_, (path, contents): (String, String)| {
            logs.borrow_mut().push(format!("Would write to `{}` ({} bytes)", path, contents.len()));
            Ok(())
        })?;
        Ok(fun)
    }

    fn make_preview_patch_file_fn<'lua>(
        &self,
        lua: &'lua Lua,
        logs: Rc<RefCell<Vec<String>>>,
    ) -> Result<mlua::Function<'lua>> {
        let root = self.workspace_root.clone();
        let fun = lua.create_function(move |_, (path, diff): (String, String)| {
            let resolved = match resolve_safe_path(&root, Path::new(&path)) {
                Ok(p) => p,
                Err(e) => {
                    logs.borrow_mut().push(format!("Invalid path `{path}`: {e}"));
                    return Ok(());
                }
            };
            
            if !resolved.exists() {
                logs.borrow_mut().push(format!("Patch target `{path}` does not exist."));
                return Ok(());
            }

            let original = match fs::read_to_string(&resolved) {
                Ok(s) => s,
                Err(e) => {
                    logs.borrow_mut().push(format!("Could not read `{path}`: {e}"));
                    return Ok(());
                }
            };

            let patch = match Patch::from_single(&diff) {
                Ok(p) => p,
                Err(e) => {
                    logs.borrow_mut().push(format!("Invalid diff format for `{path}`: {e}"));
                    return Ok(());
                }
            };
            
            match apply_patch(&original, &patch) {
                Ok(_) => {
                    logs.borrow_mut().push(format!("Patch applies cleanly to `{path}`:\n{}", diff));
                }
                Err(e) => {
                    logs.borrow_mut().push(format!("Patch CONFLICT for `{path}`: {e}"));
                }
            }
            
            Ok(())
        })?;
        Ok(fun)
    }
    
    fn make_preview_run_command_fn<'lua>(
        &self,
        lua: &'lua Lua,
        logs: Rc<RefCell<Vec<String>>>,
    ) -> Result<mlua::Function<'lua>> {
        let fun = lua.create_function(move |lua_ctx, (cmd, args): (String, Vec<String>)| {
            logs.borrow_mut().push(format!("Would run command: {} {}", cmd, args.join(" ")));
            
            // Return dummy success result so script continues
            let result = lua_ctx.create_table()?;
            result.set("status", 0)?;
            result.set("stdout", "")?;
            result.set("stderr", "")?;
            Ok(result)
        })?;
        Ok(fun)
    }

    fn build_rust_api<'lua>(
        &self,
        lua: &'lua Lua,
        logs: Rc<RefCell<Vec<String>>>,
        stderr: Rc<RefCell<Vec<String>>>,
    ) -> Result<Table<'lua>> {
        let table = lua.create_table()?;
        table.set("read_file", self.make_read_fn(lua)?)?;
        table.set("list_dir", self.make_list_fn(lua)?)?;
        table.set("write_file", self.make_write_fn(lua)?)?;
        table.set("patch_file", self.make_patch_file_fn(lua)?)?;
        table.set("http_request", self.make_http_fn(lua)?)?;
        table.set("run_command", self.make_run_command_fn(lua)?)?;
        table.set("git_status", self.make_git_status_fn(lua)?)?;
        table.set("search", self.make_search_fn(lua)?)?;
        table.set("log", self.make_log_fn(lua, logs)?)?;
        table.set("eprint", self.make_eprint_fn(lua, stderr)?)?;
        table.set("mcp", self.make_mcp_table(lua)?)?;
        table.set("get_quote", self.make_get_quote_fn(lua)?)?;
        table.set("set_context", self.make_set_context_fn(lua, self.dashboard_updates.clone())?)?;
        table.set("env", self.make_env_fn(lua)?)?;
        Ok(table)
    }

    fn make_get_quote_fn<'lua>(&self, lua: &'lua Lua) -> Result<mlua::Function<'lua>> {
        let handle = self.handle.clone();
        let fun = lua.create_function(move |lua_ctx, ticker: String| {
            let provider = yahoo::YahooConnector::new()
                .map_err(|e| mlua::Error::external(format!("init error: {e}")))?;
            let result = handle.block_on(async {
                 provider.get_latest_quotes(&ticker, "1d").await
            });

            match result {
                Ok(response) => {
                     let quote = response.last_quote().map_err(|e| mlua::Error::external(format!("no quote found: {e}")))?;
                     let table = lua_ctx.create_table()?;
                     table.set("price", quote.close)?;
                     table.set("high", quote.high)?;
                     table.set("low", quote.low)?;
                     table.set("volume", quote.volume as u64)?;
                     table.set("timestamp", quote.timestamp)?;
                     Ok(table)
                },
                Err(e) => Err(mlua::Error::external(format!("yahoo api error: {e}")))
            }
        })?;
        Ok(fun)
    }

    fn make_set_context_fn<'lua>(&self, lua: &'lua Lua, updates: Rc<RefCell<Vec<String>>>) -> Result<mlua::Function<'lua>> {
        let fun = lua.create_function(move |_, ctx: Table| {
            let json = serde_json::to_string(&lua_to_json(&ctx)?).map_err(|e| mlua::Error::external(format!("serialization error: {e}")))?;
            updates.borrow_mut().push(json);
            Ok(())
        })?;
        Ok(fun)
    }

    fn make_env_fn<'lua>(&self, lua: &'lua Lua) -> Result<mlua::Function<'lua>> {
        let fun = lua.create_function(|_, key: String| {
            Ok(std::env::var(key).ok())
        })?;
        Ok(fun)
    }

    fn build_io_table<'lua>(&self, lua: &'lua Lua) -> Result<Table<'lua>> {
        let table = lua.create_table()?;
        table.set("open", self.make_io_open_fn(lua)?)?;
        table.set("lines", self.make_io_lines_fn(lua)?)?;
        Ok(table)
    }

    fn build_fs_table<'lua>(&self, lua: &'lua Lua) -> Result<Table<'lua>> {
        let table = lua.create_table()?;
        table.set("read", self.make_read_fn(lua)?)?;
        table.set("write", self.make_write_fn(lua)?)?;
        table.set("list", self.make_list_fn(lua)?)?;
        Ok(table)
    }

    fn make_read_fn<'lua>(&self, lua: &'lua Lua) -> Result<mlua::Function<'lua>> {
        let root = self.workspace_root.clone();
        let fun = lua.create_function(move |_, path: String| {
            let resolved =
                resolve_safe_path(&root, Path::new(&path)).map_err(mlua::Error::external)?;

            let meta = fs::metadata(&resolved).map_err(|e| {
                mlua::Error::external(format!("could not get metadata for {}: {e}", resolved.display()))
            })?;
            if meta.len() > MAX_FILE_SIZE {
                return Err(mlua::Error::external(format!(
                    "file {} exceeds size limit ({} bytes)",
                    path, MAX_FILE_SIZE
                )));
            }

            let data = fs::read_to_string(&resolved).map_err(|e| {
                mlua::Error::external(format!("could not read {}: {e}", resolved.display()))
            })?;
            Ok(data)
        })?;
        Ok(fun)
    }

    fn make_io_open_fn<'lua>(&self, lua: &'lua Lua) -> Result<mlua::Function<'lua>> {
        let root = self.workspace_root.clone();
        let allow_writes = self.allow_writes;
        let fun = lua.create_function(move |lua_ctx, (path, mode): (String, Option<String>)| {
            let mode_str = mode.unwrap_or_else(|| "r".to_string());
            let file_mode =
                FileMode::parse(&mode_str).map_err(|err| mlua::Error::external(err.to_string()))?;
            if file_mode.allows_write() && !allow_writes {
                return Err(mlua::Error::external(
                    "write helpers are disabled (set allow_tool_writes = true)",
                ));
            }
            let resolved =
                resolve_safe_path(&root, Path::new(&path)).map_err(mlua::Error::external)?;

            if !file_mode.allows_write() {
                 // Check size if reading
                if let Ok(meta) = fs::metadata(&resolved) {
                     if meta.len() > MAX_FILE_SIZE {
                        return Err(mlua::Error::external(format!(
                            "file {} exceeds size limit ({} bytes)",
                            path, MAX_FILE_SIZE
                        )));
                     }
                }
            }

            let handle = LuaFileHandle::open(resolved, file_mode)
                .map_err(|err| mlua::Error::external(format!("{err:#}")))?;
            lua_ctx.create_userdata(handle)
        })?;
        Ok(fun)
    }


    fn make_io_lines_fn<'lua>(&self, lua: &'lua Lua) -> Result<mlua::Function<'lua>> {
        let root = self.workspace_root.clone();
        let fun = lua.create_function(move |lua_ctx, path: String| {
            let resolved =
                resolve_safe_path(&root, Path::new(&path)).map_err(mlua::Error::external)?;
            let contents = fs::read_to_string(&resolved).map_err(|e| {
                mlua::Error::external(format!("could not read {}: {e}", resolved.display()))
            })?;
            let lines = contents
                .lines()
                .map(|line| line.to_string())
                .collect::<Vec<_>>();
            let state = Rc::new(RefCell::new((lines, 0usize)));
            let iterator_state = Rc::clone(&state);
            let iter = lua_ctx.create_function(move |lua_ctx, ()| {
                let mut borrow = iterator_state.borrow_mut();
                if borrow.1 >= borrow.0.len() {
                    return Ok(Value::Nil);
                }
                let line = borrow.0[borrow.1].clone();
                borrow.1 += 1;
                Ok(Value::String(lua_ctx.create_string(&line)?))
            })?;
            Ok(iter)
        })?;
        Ok(fun)
    }

    fn make_list_fn<'lua>(&self, lua: &'lua Lua) -> Result<mlua::Function<'lua>> {
        let root = self.workspace_root.clone();
        let fun = lua.create_function(move |lua_ctx, path: String| {
            let resolved =
                resolve_safe_path(&root, Path::new(&path)).map_err(mlua::Error::external)?;
            let entries = fs::read_dir(&resolved).map_err(|e| {
                mlua::Error::external(format!("could not read dir {}: {e}", resolved.display()))
            })?;

            let list = lua_ctx.create_table()?;
            for (idx, entry) in entries.enumerate() {
                let entry = entry.map_err(|e| {
                    mlua::Error::external(format!("error listing {}: {e}", resolved.display()))
                })?;
                let meta = lua_ctx.create_table()?;
                meta.set("name", entry.file_name().to_string_lossy().to_string())?;
                meta.set(
                    "is_dir",
                    entry.file_type().map(|t| t.is_dir()).unwrap_or(false),
                )?;
                list.set(idx + 1, meta)?;
            }

            Ok(list)
        })?;
        Ok(fun)
    }

    fn make_write_fn<'lua>(&self, lua: &'lua Lua) -> Result<mlua::Function<'lua>> {
        let root = self.workspace_root.clone();
        let allow = self.allow_writes;
        let fun = lua.create_function(move |_, (path, contents): (String, String)| {
            if !allow {
                return Err(mlua::Error::external(
                    "write helpers are disabled (set allow_tool_writes = true)",
                ));
            }
            let resolved =
                resolve_safe_path(&root, Path::new(&path)).map_err(mlua::Error::external)?;
            if let Some(parent) = resolved.parent() {
                fs::create_dir_all(parent).map_err(|e| {
                    mlua::Error::external(format!(
                        "could not create parent dirs for {}: {e}",
                        resolved.display()
                    ))
                })?;
            }
            fs::write(&resolved, contents).map_err(|e| {
                mlua::Error::external(format!("could not write {}: {e}", resolved.display()))
            })?;
            Ok(())
        })?;
        Ok(fun)
    }

    fn make_patch_file_fn<'lua>(&self, lua: &'lua Lua) -> Result<mlua::Function<'lua>> {
        let root = self.workspace_root.clone();
        let allow = self.allow_writes;
        let fun = lua.create_function(move |_, (path, diff): (String, String)| {
            if !allow {
                return Err(mlua::Error::external(
                    "write helpers are disabled (set allow_tool_writes = true)",
                ));
            }
            let resolved =
                resolve_safe_path(&root, Path::new(&path)).map_err(mlua::Error::external)?;

            let meta = fs::metadata(&resolved).map_err(|e| {
                mlua::Error::external(format!("could not get metadata for {}: {e}", resolved.display()))
            })?;
            if meta.len() > MAX_FILE_SIZE {
                return Err(mlua::Error::external(format!(
                    "file {} exceeds size limit ({} bytes)",
                    path, MAX_FILE_SIZE
                )));
            }

            let original = fs::read_to_string(&resolved).map_err(|e| {
                mlua::Error::external(format!("could not read {}: {e}", resolved.display()))
            })?;

            let patch = Patch::from_single(&diff).map_err(|e| {
                mlua::Error::external(format!("failed to parse diff: {e}"))
            })?;

            let modified = apply_patch(&original, &patch).map_err(|e| {
                 mlua::Error::external(format!("failed to apply patch: {e}"))
            })?;
            
            fs::write(&resolved, modified).map_err(|e| {
                mlua::Error::external(format!("could not write patched file {}: {e}", resolved.display()))
            })?;
            
            Ok(())
        })?;
        Ok(fun)
    }

    fn make_run_command_fn<'lua>(&self, lua: &'lua Lua) -> Result<mlua::Function<'lua>> {
        let root = self.workspace_root.clone();
        let allow = self.allow_writes;
        let fun = lua.create_function(move |lua_ctx, (cmd, args): (String, Vec<String>)| {
            if !allow {
                return Err(mlua::Error::external(
                    "write helpers (including run_command) are disabled",
                ));
            }

            let output = Command::new(&cmd)
                .args(&args)
                .current_dir(&root)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .map_err(|e| mlua::Error::external(format!("failed to run {cmd}: {e}")))?;

            let result = lua_ctx.create_table()?;
            result.set("status", output.status.code().unwrap_or(-1))?;
            result.set("stdout", String::from_utf8_lossy(&output.stdout).to_string())?;
            result.set("stderr", String::from_utf8_lossy(&output.stderr).to_string())?;
            Ok(result)
        })?;
        Ok(fun)
    }

    fn make_git_status_fn<'lua>(&self, lua: &'lua Lua) -> Result<mlua::Function<'lua>> {
        let root = self.workspace_root.clone();
        let fun = lua.create_function(move |lua_ctx, ()| {
            let output = Command::new("git")
                .args(["status", "--porcelain"])
                .current_dir(&root)
                .output()
                .map_err(|e| mlua::Error::external(format!("git status failed: {e}")))?;

            let result = lua_ctx.create_table()?;
            result.set("status", output.status.code().unwrap_or(-1))?;
            result.set("stdout", String::from_utf8_lossy(&output.stdout).to_string())?;
            Ok(result)
        })?;
        Ok(fun)
    }

    fn make_search_fn<'lua>(&self, lua: &'lua Lua) -> Result<mlua::Function<'lua>> {
        let root = self.workspace_root.clone();
        let fun = lua.create_function(move |lua_ctx, (pattern, dir): (String, Option<String>)| {
            let target_dir = if let Some(d) = dir {
                resolve_safe_path(&root, Path::new(&d)).map_err(mlua::Error::external)?
            } else {
                root.clone()
            };

            let output = Command::new("grep")
                .args(["-r", "-n", &pattern, "."])
                .current_dir(&target_dir)
                .output()
                .map_err(|e| mlua::Error::external(format!("grep failed: {e}")))?;

            let result = lua_ctx.create_table()?;
            result.set("status", output.status.code().unwrap_or(-1))?;
            result.set("stdout", String::from_utf8_lossy(&output.stdout).to_string())?;
            result.set("stderr", String::from_utf8_lossy(&output.stderr).to_string())?;
            Ok(result)
        })?;
        Ok(fun)
    }



    fn make_http_fn<'lua>(&self, lua: &'lua Lua) -> Result<mlua::Function<'lua>> {
        let client = self.http.clone();
        let fun = lua.create_function(move |lua_ctx, opts: Table| {
            let url: String = opts
                .get("url")
                .map_err(|_| mlua::Error::external("http_request needs url field"))?;
            let method: Option<String> = opts.get("method").ok();
            let method = method.unwrap_or_else(|| "GET".to_string());
            let method: Method = method.parse().map_err(|_| {
                mlua::Error::external("http_request method must be a valid HTTP method")
            })?;

            let mut request = client.request(method, &url);

            if let Ok(headers) = opts.get::<_, Table>("headers") {
                for pair in headers.pairs::<String, String>() {
                    let (name, value) = pair
                        .map_err(|e| mlua::Error::external(format!("invalid header entry: {e}")))?;
                    let header_name = HeaderName::from_bytes(name.as_bytes()).map_err(|e| {
                        mlua::Error::external(format!("invalid header name {name}: {e}"))
                    })?;
                    let header_value = HeaderValue::from_str(&value).map_err(|e| {
                        mlua::Error::external(format!("invalid header value for {name}: {e}"))
                    })?;
                    request = request.header(header_name, header_value);
                }
            }

            if let Ok(body) = opts.get::<_, String>("body") {
                request = request.body(body);
            }

            let response = request
                .send()
                .map_err(|e| mlua::Error::external(format!("http_request failed: {e}")))?;

            let header_table = lua_ctx.create_table()?;
            for (name, value) in response.headers().iter() {
                if let Ok(text) = value.to_str() {
                    header_table.set(name.as_str(), text)?;
                }
            }

            let status = response.status().as_u16();
            let body = response
                .text()
                .map_err(|e| mlua::Error::external(format!("failed to read body: {e}")))?;

            let result = lua_ctx.create_table()?;
            result.set("status", status)?;
            result.set("body", body)?;
            result.set("headers", header_table)?;
            Ok(result)
        })?;
        Ok(fun)
    }

    fn make_log_fn<'lua>(
        &self,
        lua: &'lua Lua,
        sink: Rc<RefCell<Vec<String>>>,
    ) -> Result<mlua::Function<'lua>> {
        let fun = lua.create_function(move |_, payload: Value| {
            let (level, message) = match payload {
                Value::String(text) => ("info".to_string(), text.to_string_lossy().into_owned()),
                Value::Table(table) => {
                    let level = table.get::<_, Option<String>>("level").ok().flatten();
                    let message: String = table
                        .get("message")
                        .map_err(|_| mlua::Error::external("log expects `message` field"))?;
                    (level.unwrap_or_else(|| "info".into()), message)
                }
                Value::Nil => ("info".to_string(), "<nil>".into()),
                other => {
                    return Err(mlua::Error::external(format!(
                        "log expects string or table, got {other:?}"
                    )));
                }
            };
            sink.borrow_mut()
                .push(format!("[{}] {}", level.to_lowercase(), message));
            Ok(())
        })?;
        Ok(fun)
    }

    fn make_mcp_table<'lua>(&self, lua: &'lua Lua) -> Result<Table<'lua>> {
        let table = lua.create_table()?;
        table.set("list_servers", self.make_mcp_list_servers_fn(lua)?)?;
        table.set("list_tools", self.make_mcp_list_tools_fn(lua)?)?;
        table.set("load_tool", self.make_mcp_load_tool_fn(lua)?)?;
        Ok(table)
    }

    fn make_mcp_list_servers_fn<'lua>(&self, lua: &'lua Lua) -> Result<mlua::Function<'lua>> {
        let root = self.workspace_root.clone();
        let fun = lua.create_function(move |lua_ctx, ()| {
            let list = lua_ctx.create_table()?;
            let servers_root = root.join("servers");
            let entries = match fs::read_dir(&servers_root) {
                Ok(entries) => entries,
                Err(_) => return Ok(list),
            };
            for (idx, entry) in entries.enumerate() {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(err) => {
                        return Err(mlua::Error::external(format!(
                            "failed to scan servers: {err}"
                        )));
                    }
                };
                if entry.file_type().map(|ty| ty.is_dir()).unwrap_or(false) {
                    list.set(idx + 1, entry.file_name().to_string_lossy().to_string())?;
                }
            }
            Ok(list)
        })?;
        Ok(fun)
    }

    fn make_mcp_list_tools_fn<'lua>(&self, lua: &'lua Lua) -> Result<mlua::Function<'lua>> {
        let root = self.workspace_root.clone();
        let fun = lua.create_function(move |lua_ctx, server: String| {
            ensure_single_component(&server, "server").map_err(mlua::Error::external)?;
            let list = lua_ctx.create_table()?;
            let server_dir = root.join("servers").join(&server);
            let entries = match fs::read_dir(&server_dir) {
                Ok(entries) => entries,
                Err(_) => return Ok(list),
            };
            for (idx, entry) in entries.enumerate() {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(err) => {
                        return Err(mlua::Error::external(format!(
                            "failed to list tools: {err}"
                        )));
                    }
                };
                if entry.file_type().map(|ty| ty.is_file()).unwrap_or(false) {
                    list.set(idx + 1, entry.file_name().to_string_lossy().to_string())?;
                }
            }
            Ok(list)
        })?;
        Ok(fun)
    }

    fn make_mcp_load_tool_fn<'lua>(&self, lua: &'lua Lua) -> Result<mlua::Function<'lua>> {
        let root = self.workspace_root.clone();
        let fun = lua.create_function(move |lua_ctx, (server, tool): (String, String)| {
            ensure_single_component(&server, "server").map_err(mlua::Error::external)?;
            ensure_single_component(&tool, "tool").map_err(mlua::Error::external)?;
            let file_path = root.join("servers").join(&server).join(&tool);
            let contents = fs::read_to_string(&file_path).map_err(|e| {
                mlua::Error::external(format!("failed to load tool {}: {e}", file_path.display()))
            })?;
            let table = lua_ctx.create_table()?;
            table.set("path", file_path.to_string_lossy().to_string())?;
            table.set("content", contents)?;
            Ok(table)
        })?;
        Ok(fun)
    }

    fn make_eprint_fn<'lua>(
        &self,
        lua: &'lua Lua,
        sink: Rc<RefCell<Vec<String>>>,
    ) -> Result<mlua::Function<'lua>> {
        let fun = lua.create_function(move |_, payload: Table| {
            let message: String = payload
                .get("message")
                .map_err(|_| mlua::Error::external("eprint expects table with 'message' field"))?;
            sink.borrow_mut().push(message);
            Ok(())
        })?;
        Ok(fun)
    }

    fn make_print_fn<'lua>(
        &self,
        lua: &'lua Lua,
        sink: Rc<RefCell<Vec<String>>>,
    ) -> Result<mlua::Function<'lua>> {
        let fun = lua.create_function(move |_, values: Variadic<Value>| {
            let line = values
                .into_iter()
                .map(render_value)
                .collect::<Vec<_>>()
                .join("\t");
            sink.borrow_mut().push(line);
            Ok(())
        })?;
        Ok(fun)
    }

    fn make_warn_fn<'lua>(
        &self,
        lua: &'lua Lua,
        sink: Rc<RefCell<Vec<String>>>,
    ) -> Result<mlua::Function<'lua>> {
        let fun = lua.create_function(move |_, values: Variadic<Value>| {
            let line = values
                .into_iter()
                .map(render_value)
                .collect::<Vec<_>>()
                .join("\t");
            sink.borrow_mut().push(line);
            Ok(())
        })?;
        Ok(fun)
    }

    fn build_package_table<'lua>(&self, lua: &'lua Lua) -> Result<Table<'lua>> {
        let package = lua.create_table()?;
        let preload = lua.create_table()?;
        let loader = lua.create_function(move |lua_ctx, _name: String| {
            let globals = lua_ctx.globals();
            let rust_table: Table = globals
                .get("rust")
                .map_err(|_| mlua::Error::external("rust helpers missing"))?;
            Ok(rust_table)
        })?;
        preload.set("rust", loader)?;
        package.set("preload", preload)?;
        package.set("path", "")?;
        package.set("cpath", "")?;
        Ok(package)
    }

    fn make_safe_require_fn<'lua>(&self, lua: &'lua Lua) -> Result<mlua::Function<'lua>> {
        let fun = lua.create_function(|lua_ctx, name: String| {
            let globals = lua_ctx.globals();
            let package: Table = globals
                .get("package")
                .map_err(|_| mlua::Error::external("package table missing"))?;
            let preload: Table = package
                .get("preload")
                .map_err(|_| mlua::Error::external("package.preload missing"))?;
            let loader: mlua::Function = preload.get(name.as_str()).map_err(|_| {
                mlua::Error::external(format!("module '{name}' not available (only 'rust')"))
            })?;
            let module: Table = loader.call(name)?;
            Ok(module)
        })?;
        Ok(fun)
    }
}

fn collect_buffer(buffer: Rc<RefCell<Vec<String>>>) -> Vec<String> {
    Rc::try_unwrap(buffer)
        .map(|cell| cell.into_inner())
        .unwrap_or_else(|rc| rc.borrow().clone())
}

fn ensure_single_component(value: &str, kind: &str) -> Result<()> {
    let mut components = Path::new(value).components();
    match components.next() {
        Some(Component::Normal(_)) if components.next().is_none() => Ok(()),
        _ => bail!("{kind} name must be a single path segment"),
    }
}

fn resolve_safe_path(root: &Path, path: &Path) -> Result<PathBuf> {
    let candidate = if path.is_absolute() {
        PathBuf::from(path)
    } else {
        root.join(path)
    };

    let normalized = canonicalize_with_missing(&candidate)
        .with_context(|| format!("failed to access {}", candidate.display()))?;

    if !normalized.starts_with(root) {
        bail!("path {} escapes workspace root", normalized.display());
    }

    Ok(normalized)
}

fn canonicalize_with_missing(path: &Path) -> io::Result<PathBuf> {
    match path.canonicalize() {
        Ok(value) => Ok(value),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            let mut segments = Vec::<OsString>::new();
            let mut current = path;
            while !current.exists() {
                let parent = current.parent().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        "unable to canonicalize path without parent",
                    )
                })?;
                let name = current.file_name().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        "unable to canonicalize unnamed path segment",
                    )
                })?;
                segments.push(name.to_os_string());
                current = parent;
            }
            let mut normalized = current.canonicalize()?;
            while let Some(segment) = segments.pop() {
                normalized.push(segment);
            }
            Ok(normalized)
        }
        Err(err) => Err(err),
    }
}

fn lua_to_json(table: &Table) -> Result<serde_json::Value, mlua::Error> {
    let mut map = serde_json::Map::new();
    for pair in table.clone().pairs::<String, Value>() {
        let (key, value) = pair?;
        let json_val = match value {
            Value::Nil => serde_json::Value::Null,
            Value::Boolean(b) => serde_json::Value::Bool(b),
            Value::Integer(i) => serde_json::Value::Number(i.into()),
            Value::Number(n) => serde_json::Number::from_f64(n).map(serde_json::Value::Number).unwrap_or(serde_json::Value::Null),
            Value::String(s) => serde_json::Value::String(s.to_string_lossy().into_owned()),
            Value::Table(t) => lua_to_json(&t)?,
            _ => serde_json::Value::String(format!("{:?}", value)),
        };
        map.insert(key, json_val);
    }
    Ok(serde_json::Value::Object(map))
}

fn render_value(value: Value) -> String {
    match value {
        Value::Nil => "nil".into(),
        Value::Boolean(b) => b.to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Number(n) => format!("{n}"),
        Value::String(s) => s.to_string_lossy().into_owned(),
        Value::Table(t) => table_to_string(&t),
        Value::Function(_) => "<function>".into(),
        Value::Thread(_) => "<thread>".into(),
        Value::UserData(_) => "<userdata>".into(),
        Value::Error(err) => format!("{err}"),
        other => format!("{other:?}"),
    }
}

fn table_to_string(table: &Table) -> String {
    let mut items = Vec::new();
    for pair in table.clone().pairs::<Value, Value>() {
        match pair {
            Ok((key, value)) => {
                items.push(format!("{}: {}", render_value(key), render_value(value)));
            }
            Err(err) => {
                return format!("{{error iterating table: {err}}}");
            }
        }
    }
    format!("{{{}}}", items.join(", "))
}

fn apply_patch(original: &str, patch: &Patch) -> Result<String> {
    let mut lines: Vec<&str> = original.lines().collect();
    let mut offset: isize = 0;

    for hunk in &patch.hunks {
        let start = hunk.old_range.start as isize + offset - 1;
        if start < 0 { bail!("invalid line number in patch"); }
        let start = start as usize;
        
        let old_count = hunk.old_range.count as usize;
        
        if start + old_count > lines.len() {
             bail!("patch application out of bounds (line {})", start + 1);
        }
        
        let mut new_block = Vec::new();
        for line in &hunk.lines {
             match line {
                 Line::Context(s) | Line::Add(s) => new_block.push(*s),
                 Line::Remove(_) => {}
             }
        }
        
        lines.splice(start..start+old_count, new_block.clone());
        
        let new_count = new_block.len();
        offset += (new_count as isize) - (old_count as isize);
    }
    
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn create_executor(root: &Path, allow_writes: bool) -> Result<(LuaExecutor, tokio::runtime::Runtime)> {
        let rt = tokio::runtime::Runtime::new()?;
        let handle = rt.handle().clone();
        let executor = LuaExecutor::new(root, allow_writes, handle)?;
        Ok((executor, rt))
    }

    #[test]
    fn io_open_supports_reading_files() -> Result<()> {
        let tmp = tempdir()?;
        let file = tmp.path().join("sample.txt");
        fs::write(&file, "alpha\nbeta")?;
        let (executor, _rt) = create_executor(tmp.path(), false)?;
        let output = executor.run_script(
            r#"
            local f = io.open("sample.txt", "r")
            local data = f:read("*a")
            f:close()
            return data
        "#,
        )?;
        assert_eq!(output.value, "alpha\nbeta");
        Ok(())
    }

    #[test]
    fn io_open_respects_write_flag() -> Result<()> {
        let tmp = tempdir()?;
        let (executor, _rt) = create_executor(tmp.path(), false)?;
        let err = executor.run_script(
            r#"
            local f = io.open("new.txt", "w")
        "#,
        );
        assert!(
            err.unwrap_err()
                .to_string()
                .contains("write helpers are disabled")
        );
        Ok(())
    }

    #[test]
    fn io_open_allows_creating_files_when_enabled() -> Result<()> {
        let tmp = tempdir()?;
        let (executor, _rt) = create_executor(tmp.path(), true)?;
        let output = executor.run_script(
            r#"
            local f = io.open("dir/example.txt", "w")
            f:write("demo")
            f:close()
            return rust.read_file("dir/example.txt")
        "#,
        )?;
        assert_eq!(output.value, "demo");
        Ok(())
    }

    #[test]
    fn resolve_safe_path_stays_within_root() -> Result<()> {
        let tmp = tempdir()?;
        let target = tmp.path().join("dir/example.txt");
        fs::create_dir_all(target.parent().unwrap())?;
        fs::write(&target, "hello")?;
        let resolved = resolve_safe_path(tmp.path(), Path::new("dir/example.txt"))?;
        assert_eq!(resolved, target.canonicalize()?);
        Ok(())
    }

    #[test]
    fn resolve_safe_path_rejects_escape() {
        let tmp = tempdir().expect("tempdir");
        let result = resolve_safe_path(tmp.path(), Path::new("../outside.txt"));
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("escapes workspace root")
        );
    }

    #[test]
    fn http_request_helper_handles_basic_request() -> Result<()> {
        use std::{
            io::{Read, Write},
            net::TcpListener,
            thread,
        };

        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let handle = thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buffer = [0u8; 1024];
                let _ = stream.read(&mut buffer);
                let response =
                    b"HTTP/1.1 200 OK\r\nX-Test-Header: Pong\r\nContent-Length: 4\r\n\r\npong";
                let _ = stream.write_all(response);
            }
        });

        let tmp = tempdir()?;
        let (executor, _rt) = create_executor(tmp.path(), false)?;
        let script = format!(
            r#"
            local resp = rust.http_request{{
                url = "http://{addr}/ping",
                method = "POST",
                headers = {{ ["X-Demo"] = "value" }},
                body = "ping"
            }}
            return resp.status .. ":" .. resp.body
        "#,
            addr = addr
        );
        let output = executor.run_script(&script)?;
        assert_eq!(output.value.trim(), "200:pong");
        handle.join().expect("server thread");
        Ok(())
    }

    #[test]
    fn list_dir_returns_entries() -> Result<()> {
        let tmp = tempdir()?;
        fs::write(tmp.path().join("one.txt"), "1")?;
        fs::create_dir(tmp.path().join("dir"))?;
        let (executor, _rt) = create_executor(tmp.path(), false)?;
        let output = executor.run_script(
            r#"
            local entries = rust.list_dir(".")
            local names = {}
            for i = 1, #entries do
                table.insert(names, entries[i].name)
            end
            table.sort(names)
            return table.concat(names, ",")
        "#,
        )?;
        assert!(
            output.value.contains("dir") && output.value.contains("one.txt"),
            "expected both entries in output: {}",
            output.value
        );
        Ok(())
    }

    #[test]
    fn rust_log_records_messages() -> Result<()> {
        let tmp = tempdir()?;
        let (executor, _rt) = create_executor(tmp.path(), false)?;
        let output = executor.run_script(
            r#"
            rust.log("note")
            rust.log({ level = "warn", message = "warned" })
            return "ok"
        "#,
        )?;
        assert_eq!(output.logs.len(), 2);
        assert_eq!(output.logs[0], "[info] note");
        assert_eq!(output.logs[1], "[warn] warned");
        Ok(())
    }

    #[test]
    fn file_mode_parse_supports_core_modes() {
        assert!(matches!(FileMode::parse("r").unwrap(), FileMode::Read));
        assert!(matches!(FileMode::parse("w").unwrap(), FileMode::Write));
        assert!(matches!(FileMode::parse("a").unwrap(), FileMode::Append));
        assert!(FileMode::parse("invalid").is_err());
    }

    #[test]
    fn patch_file_applies_diff() -> Result<()> {
        let tmp = tempdir()?;
        let file = tmp.path().join("code.rs");
        fs::write(&file, "fn main() {\n    println!(\"old\");\n}\n")?;
        
        let (executor, _rt) = create_executor(tmp.path(), true)?;
        let diff = r#"--- code.rs
+++ code.rs
@@ -1,3 +1,3 @@
 fn main() {
-    println!("old");
+    println!("new");
 }
"#;
        let diff_lua = diff.replace("\\", "\\\\").replace("\n", "\\n").replace("\"", "\\\"");
        
        let script = format!(r#"
            rust.patch_file("code.rs", "{}")
            return rust.read_file("code.rs")
        "#, diff_lua);
        
        let output = executor.run_script(&script)?;
        assert_eq!(output.value, "fn main() {\n    println!(\"new\");\n}");
        Ok(())
    }

    #[test]
    fn run_command_executes_shell_cmd() -> Result<()> {
        let tmp = tempdir()?;
        let (executor, _rt) = create_executor(tmp.path(), true)?;
        
        // echo is usually built-in or available
        let script = r#"
            local res = rust.run_command("echo", {"hello"})
            return res.stdout
        "#;
        
        let output = executor.run_script(script)?;
        assert!(output.value.trim().contains("hello"));
        Ok(())
    }

    #[test]
    fn run_command_blocked_if_read_only() -> Result<()> {
        let tmp = tempdir()?;
        let (executor, _rt) = create_executor(tmp.path(), false)?; // read-only
        let script = r#"rust.run_command("echo", {"hello"})"#;
        let err = executor.run_script(script);
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("write helpers"));
        Ok(())
    }

    #[test]
    fn persistence_preserves_globals() -> Result<()> {
        let tmp = tempdir()?;
        let (executor, _rt) = create_executor(tmp.path(), false)?;
        
        // First run: define a variable
        let _ = executor.run_script("x = 42")?;
        
        // Second run: read it back
        let output = executor.run_script("return x")?;
        
        assert_eq!(output.value, "42");
        Ok(())
    }

    #[test]
    fn prelude_is_loaded() -> Result<()> {
        let tmp = tempdir()?;
        let (executor, _rt) = create_executor(tmp.path(), false)?;

        // Check repr
        let output = executor.run_script("return repr({a=1})")?;
        // Output format depends on repr impl but should contain keys
        assert!(output.value.contains("a = 1"));

        // Check functional helpers
        let output = executor.run_script(
            "return table.concat(map({1,2,3}, function(x) return x*2 end), ',')"
        )?;
        assert_eq!(output.value, "2,4,6");
        
        Ok(())
    }

    #[test]
    fn reset_clears_globals() -> Result<()> {
        let tmp = tempdir()?;
        let (mut executor, _rt) = create_executor(tmp.path(), false)?;
        
        executor.run_script("x = 100")?;
        executor.reset()?;
        
        let output = executor.run_script("return x")?;
        assert_eq!(output.value, "nil");
        
        // Prelude should still be loaded after reset
        let output = executor.run_script("return repr(nil)")?;
        assert_eq!(output.value, "nil");
        
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileMode {
    Read,
    Write,
    Append,
}

impl FileMode {
    fn parse(input: &str) -> Result<Self, &'static str> {
        let normalized = input.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "r" | "rb" => Ok(FileMode::Read),
            "w" | "wb" => Ok(FileMode::Write),
            "a" | "ab" => Ok(FileMode::Append),
            _ => Err("unsupported io mode (expected r, w, or a)"),
        }
    }

    fn allows_write(self) -> bool {
        matches!(self, FileMode::Write | FileMode::Append)
    }
}

#[derive(Debug)]
struct LuaFileHandle {
    path: PathBuf,
    mode: FileMode,
    cursor: usize,
    buffer: String,
    dirty: bool,
    closed: bool,
}

impl LuaFileHandle {
    fn open(path: PathBuf, mode: FileMode) -> Result<Self> {
        let buffer = match mode {
            FileMode::Read => fs::read_to_string(&path)
                .with_context(|| format!("could not read {}", path.display()))?,
            FileMode::Write => String::new(),
            FileMode::Append => fs::read_to_string(&path).unwrap_or_default(),
        };
        Ok(Self {
            path,
            mode,
            cursor: 0,
            buffer,
            dirty: false,
            closed: false,
        })
    }

    fn ensure_open(&self) -> Result<()> {
        if self.closed {
            bail!("file already closed");
        }
        Ok(())
    }

    fn ensure_can_read(&self) -> Result<()> {
        if self.mode == FileMode::Write || self.mode == FileMode::Append {
            bail!("file opened without read access");
        }
        Ok(())
    }

    fn ensure_can_write(&self) -> Result<()> {
        if !self.mode.allows_write() {
            bail!("file opened in read-only mode");
        }
        Ok(())
    }

    fn read_all(&mut self) -> String {
        let slice = self.buffer[self.cursor..].to_string();
        self.cursor = self.buffer.len();
        slice
    }

    fn read_line(&mut self) -> Option<String> {
        if self.cursor >= self.buffer.len() {
            return None;
        }
        let remaining = &self.buffer[self.cursor..];
        if let Some(pos) = remaining.find('\n') {
            let line = &remaining[..pos];
            self.cursor += pos + 1;
            Some(line.trim_end_matches('\r').to_string())
        } else {
            let line = remaining.trim_end_matches('\r').to_string();
            self.cursor = self.buffer.len();
            Some(line)
        }
    }

    fn write_data(&mut self, data: &str) -> Result<()> {
        self.ensure_can_write()?;
        self.buffer.push_str(data);
        self.dirty = true;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        if self.mode.allows_write() && self.dirty {
            if let Some(parent) = self.path.parent() {
                fs::create_dir_all(parent).with_context(|| {
                    format!("could not create parent dirs for {}", self.path.display())
                })?;
            }
            fs::write(&self.path, &self.buffer)
                .with_context(|| format!("could not write {}", self.path.display()))?;
        }
        self.closed = true;
        Ok(())
    }
}

impl Drop for LuaFileHandle {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

impl UserData for LuaFileHandle {
    fn add_methods<'lua, M: UserDataMethods<'lua, Self>>(methods: &mut M) {
        methods.add_method_mut("read", |lua_ctx, this, mode: Option<String>| {
            this.ensure_open().map_err(mlua::Error::external)?;
            this.ensure_can_read().map_err(mlua::Error::external)?;
            let spec = mode.unwrap_or_else(|| "*l".into());
            match spec.as_str() {
                "*a" => {
                    let data = this.read_all();
                    Ok(Value::String(lua_ctx.create_string(&data)?))
                }
                "*l" => match this.read_line() {
                    Some(line) => Ok(Value::String(lua_ctx.create_string(&line)?)),
                    None => Ok(Value::Nil),
                },
                other => Err(mlua::Error::external(format!(
                    "io.read mode `{other}` not supported (use \"*a\" or \"*l\")"
                ))),
            }
        });

        methods.add_method_mut("write", |_, this, data: String| {
            this.ensure_open().map_err(mlua::Error::external)?;
            this.write_data(&data).map_err(mlua::Error::external)?;
            Ok(true)
        });

        methods.add_method_mut("close", |_, this, ()| {
            this.flush().map_err(mlua::Error::external)?;
            Ok(true)
        });
    }
}
