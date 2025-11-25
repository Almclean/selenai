mod app;
mod config;
mod llm;
mod lua_tool;
mod macros;
mod session;
mod tui;
mod types;

use std::{env, io};

use anyhow::Result;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

fn main() -> Result<()> {
    load_env_file()?;
    init_tracing();
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

fn init_tracing() {
    let val = env::var("SELENAI_TRACE").unwrap_or_default();
    if val.is_empty() {
        return;
    }

    let format = if val == "pretty" {
        fmt::format().pretty().with_thread_ids(true).compact()
    } else {
        fmt::format().compact().with_thread_ids(true).compact()
    };

    // We default to writing to stderr. Users should redirect stderr to a file
    // if running with the TUI enabled, e.g.: `SELENAI_TRACE=info cargo run 2> log`
    let layer = fmt::layer()
        .event_format(format)
        .with_writer(io::stderr);

    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap();

    tracing_subscriber::registry()
        .with(filter)
        .with(layer)
        .init();
}
