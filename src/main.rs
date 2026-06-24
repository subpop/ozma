mod action;
mod acp;
mod app;
mod cli;
mod components;
mod config;
mod errors;
mod event;
mod export;
mod logging;
mod mcp;
mod model;
mod persona;
mod pricing;
mod provider;
mod store;
mod tools;
mod tui;
mod ui;

use clap::Parser;
use color_eyre::eyre::Result;
use tracing::info;

use crate::app::App;
use crate::cli::Cli;
use crate::config::{Config, SourceKind};
use crate::mcp::McpManager;
use crate::provider::Provider;
use crate::store::Store;
use crate::tui::Tui;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize error handling (must be first -- sets up panic hooks)
    errors::init()?;

    // Initialize logging to file
    logging::init()?;

    // Parse command line arguments
    let cli = Cli::parse();

    // Load configuration
    let config = Config::load()?;

    // Open the persistent store
    let store = Store::open()?;

    // Create and run the application
    let tui = Tui::new()?
        .tick_rate(cli.tick_rate)
        .frame_rate(cli.frame_rate);

    match config.active_source() {
        Some(SourceKind::Agent(_)) => {
            info!("Starting with ACP agent source");
            let mut app = App::new_for_agent(store, config).await;
            app.run(tui).await?;
        }
        _ => {
            // Direct provider path — connect MCP in-process
            let provider = Provider::from_config(&config).await?;
            let mut mcp_manager = McpManager::new();
            if !config.mcp_servers.is_empty() {
                info!(
                    "Connecting to {} MCP server(s)...",
                    config.mcp_servers.len()
                );
                mcp_manager.connect_all(&config.mcp_servers).await;
            }
            let mut app = App::new(provider, store, config, mcp_manager);
            app.run(tui).await?;
        }
    }

    Ok(())
}
