mod app;
mod config;
mod llm;
mod lua_tool;
mod tui;
mod types;

use anyhow::Result;

fn main() -> Result<()> {
    let mut app = app::App::new()?;
    app.run()
}
