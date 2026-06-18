use std::fs;

use anyhow::{Context, Result};
use tracing_subscriber::EnvFilter;

pub fn init_tracing() -> Result<()> {
    dotenvy::dotenv().ok();

    let log_path = dirs::data_local_dir().context("Could not get local directory")?;

    let log_path = log_path.join("imgfind");

    fs::create_dir_all(&log_path).context("Could not create log directory")?;

    let file_appender = tracing_appender::rolling::never(&log_path, "log.txt");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    // Honor RUST_LOG when set; default to `info`.  Additionally suppress
    // rawler's per-file WARN noise ("Decoder has no full/preview image
    // support") that fires on every RAW file lacking an embedded preview —
    // the demosaic fallback handles those files correctly without a warning.
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"))
        .add_directive("rawler=error".parse()?);

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(non_blocking)
        .with_ansi(true)
        .init();

    std::mem::forget(_guard);

    Ok(())
}
