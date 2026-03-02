//! rproxy-agent — residential exit-node agent.
//!
//! Connects outbound to the proxy server via WebSocket, authenticates,
//! then accepts yamux streams and relays each one to the requested target.

use anyhow::Result;
use clap::Parser;
use tracing::info;

mod config;
mod tunnel;

use config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("rproxy_agent=info")),
        )
        .init();

    let cfg = Config::parse();
    info!("starting rproxy-agent");
    info!("  server = {}", cfg.server_url);

    loop {
        if let Err(e) = tunnel::run(&cfg).await {
            tracing::warn!("tunnel error: {:#}; reconnecting in 5s", e);
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}
