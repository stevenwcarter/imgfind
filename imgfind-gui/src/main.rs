slint::include_modules!();

#[allow(dead_code)] // state items will be wired to the Slint UI in later tasks
mod state;
#[allow(dead_code)] // backend will be wired to the Slint UI in later tasks
mod backend;

fn main() -> anyhow::Result<()> {
    let window = MainWindow::new()?;
    window.run()?;
    Ok(())
}
