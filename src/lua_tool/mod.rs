use std::{
    cell::RefCell,
    fs,
    path::{Component, Path, PathBuf},
    rc::Rc,
};

use anyhow::{Context, Result, bail};
use mlua::{Lua, LuaOptions, StdLib, Table, Value, Variadic};
use reqwest::{Method, blocking::Client, header::HeaderName, header::HeaderValue};

pub struct LuaExecutor {
    workspace_root: PathBuf,
    allow_writes: bool,
    http: Client,
}

#[derive(Debug, Clone)]
pub struct LuaExecution {
    pub value: String,
    pub logs: Vec<String>,
    pub stdout: Vec<String>,
    pub stderr: Vec<String>,
}

impl LuaExecutor {
    pub fn new(root: impl Into<PathBuf>, allow_writes: bool) -> Result<Self> {
        let root = root.into();
        let canonical = if root.exists() {
            root.canonicalize()
                .with_context(|| format!("failed to canonicalize {}", root.display()))?
        } else {
            root
        };

        let http = Client::builder().build()?;

        Ok(Self {
            workspace_root: canonical,
            allow_writes,
            http,
        })
    }

    pub fn run_script(&self, script: &str) -> Result<LuaExecution> {
        let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default())?;
        let logs = Rc::new(RefCell::new(Vec::new()));
        let stdout = Rc::new(RefCell::new(Vec::new()));
        let stderr = Rc::new(RefCell::new(Vec::new()));
        let rust_api = self.build_rust_api(&lua, logs.clone(), stderr.clone())?;
        let globals = lua.globals();
        let _ = globals.raw_set("io", Value::Nil);
        let _ = globals.raw_set("os", Value::Nil);
        globals.set("print", self.make_print_fn(&lua, stdout.clone())?)?;
        globals.set("warn", self.make_warn_fn(&lua, stderr.clone())?)?;
        globals.set("rust", rust_api.clone())?;
        let package = self.build_package_table(&lua)?;
        globals.set("package", package)?;
        globals.set("require", self.make_safe_require_fn(&lua)?)?;

        let value = lua.load(script).set_name("tool").eval::<Value>()?;
        let logs = collect_buffer(logs);
        let stdout = collect_buffer(stdout);
        let stderr = collect_buffer(stderr);
        Ok(LuaExecution {
            value: render_value(value),
            logs,
            stdout,
            stderr,
        })
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
        table.set("http_request", self.make_http_fn(lua)?)?;
        table.set("log", self.make_log_fn(lua, logs)?)?;
        table.set("eprint", self.make_eprint_fn(lua, stderr)?)?;
        table.set("mcp", self.make_mcp_table(lua)?)?;
        Ok(table)
    }

    fn make_read_fn<'lua>(&self, lua: &'lua Lua) -> Result<mlua::Function<'lua>> {
        let root = self.workspace_root.clone();
        let fun = lua.create_function(move |_, path: String| {
            let resolved =
                resolve_safe_path(&root, Path::new(&path)).map_err(mlua::Error::external)?;
            let data = fs::read_to_string(&resolved).map_err(|e| {
                mlua::Error::external(format!("could not read {}: {e}", resolved.display()))
            })?;
            Ok(data)
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

    let normalized = candidate
        .canonicalize()
        .with_context(|| format!("failed to access {}", candidate.display()))?;

    if !normalized.starts_with(root) {
        bail!("path {} escapes workspace root", normalized.display());
    }

    Ok(normalized)
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
