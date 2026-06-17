slint::include_modules!();

fn main() -> anyhow::Result<()> {
    let window = MainWindow::new()?;
    window.run()?;
    Ok(())
}
