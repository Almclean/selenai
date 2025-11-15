mod app;
mod config;
mod llm;
mod lua_tool;
mod session;
mod tui;
mod types;

use std::io;

use anyhow::Result;

fn main() -> Result<()> {
    load_env_file()?;
    let mut app = app::App::new()?;
    app.run()
}

fn load_env_file() -> Result<()> {
    match dotenvy::dotenv() {
        Ok(_) => Ok(()),
        Err(dotenvy::Error::Io(err)) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}
