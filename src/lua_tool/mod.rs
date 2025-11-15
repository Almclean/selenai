use std::{
    cell::RefCell,
    ffi::OsString,
    fs, io,
    path::{Component, Path, PathBuf},
    rc::Rc,
};

use anyhow::{Context, Result, bail};
use mlua::{Lua, LuaOptions, StdLib, Table, UserData, UserDataMethods, Value, Variadic};
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
        let _ = globals.raw_set("os", Value::Nil);
        globals.set("print", self.make_print_fn(&lua, stdout.clone())?)?;
        globals.set("warn", self.make_warn_fn(&lua, stderr.clone())?)?;
        globals.set("rust", rust_api.clone())?;
        globals.set("io", self.build_io_table(&lua)?)?;
        globals.set("fs", self.build_fs_table(&lua)?)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn io_open_supports_reading_files() -> Result<()> {
        let tmp = tempdir()?;
        let file = tmp.path().join("sample.txt");
        fs::write(&file, "alpha\nbeta")?;
        let executor = LuaExecutor::new(tmp.path(), false)?;
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
        let executor = LuaExecutor::new(tmp.path(), false)?;
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
        let executor = LuaExecutor::new(tmp.path(), true)?;
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
