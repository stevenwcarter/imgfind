use anyhow::Result;

use crate::database::Database;

mod app;
mod event;
mod ui;
mod widget;

pub async fn tui(db: Database) -> Result<()> {
    let terminal = ratatui::init();
    let result = app::App::new(db)?.run(terminal).await;
    ratatui::restore();
    result
}
