use std::io::stdout;

use anyhow::Result;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen},
};

use crate::database::Database;

mod app;
mod event;
mod ui;
mod widget;

#[cfg(test)]
mod render_tests;

pub async fn tui(db: Database) -> Result<()> {
    let terminal = ratatui::init();
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture).unwrap();
    let result = app::App::new(db)?.run(terminal).await;
    ratatui::restore();
    execute!(stdout, LeaveAlternateScreen, DisableMouseCapture).unwrap();
    result
}
