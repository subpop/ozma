use color_eyre::eyre::Result;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// Initialize tracing with a file appender.
///
/// Logs are written to `~/.local/share/ozma/ozma.log`. The TUI owns stderr/stdout,
/// so we must log to a file instead.
pub fn init() -> Result<()> {
    let data_dir = dirs();
    std::fs::create_dir_all(&data_dir)?;

    let log_file = std::fs::File::create(data_dir.join("ozma.log"))?;

    let file_layer = fmt::layer()
        .with_writer(log_file)
        .with_target(true)
        .with_thread_ids(false)
        .with_ansi(false);

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("ozma=debug,warn"));

    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .init();

    tracing::info!("ozma v{} starting", env!("CARGO_PKG_VERSION"));

    Ok(())
}

/// Returns the data directory for ozma, via the `dirs` crate.
fn dirs() -> std::path::PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("ozma")
}
