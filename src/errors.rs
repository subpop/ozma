use color_eyre::eyre::Result;

/// Initialize color_eyre with custom panic and error hooks that restore the terminal
/// before printing diagnostics. Without this, a panic leaves the terminal in raw mode.
pub fn init() -> Result<()> {
    let (panic_hook, eyre_hook) = color_eyre::config::HookBuilder::default()
        .panic_section(format!(
            "This is a bug. Consider reporting it at: {}",
            env!("CARGO_PKG_NAME")
        ))
        .into_hooks();

    // Install the eyre hook for color_eyre error reports
    eyre_hook.install()?;

    // Install a custom panic hook that restores the terminal first
    std::panic::set_hook(Box::new(move |info| {
        // Best-effort terminal restoration before printing the panic
        if let Err(e) = crate::tui::restore_terminal() {
            eprintln!("Failed to restore terminal during panic: {e}");
        }
        // Now print the panic report via color_eyre's panic hook
        eprintln!("{}", panic_hook.panic_report(info));
    }));

    Ok(())
}
