slint::include_modules!();

#[allow(dead_code)] // state items will be wired to the Slint UI in later tasks
mod state;

fn main() -> anyhow::Result<()> {
    let window = MainWindow::new()?;
    window.run()?;
    Ok(())
}
