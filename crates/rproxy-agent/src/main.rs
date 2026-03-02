//! rproxy-agent — residential exit-node agent.
//!
//! Connects outbound to the proxy server via WebSocket, authenticates,
//! then accepts yamux streams and relays each one to the requested target.
//!
//! Reconnects automatically with exponential backoff on any failure.

use anyhow::Result;
use clap::Parser;
use std::time::Duration;
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

    // Exponential backoff: 1s → 2s → 4s → ... capped at 60s.
    let mut backoff = Duration::from_secs(1);
    loop {
        match tunnel::run(&cfg).await {
            Ok(()) => {
                tracing::info!("tunnel closed cleanly; reconnecting in {}s", backoff.as_secs());
            }
            Err(e) => {
                tracing::warn!(
                    "tunnel error: {:#}; reconnecting in {}s",
                    e,
                    backoff.as_secs()
                );
            }
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}
