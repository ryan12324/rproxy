//! rproxy-server — residential proxy gateway.
//!
//! Listens on:
//!   - SOCKS5 port (default 1080) for client proxy requests
//!   - HTTP CONNECT port (default 8080) for client proxy requests
//!   - WebSocket port (default 8888) for provider agent connections
//!
//! Provider agents connect via WSS, authenticate with a shared token,
//! and are multiplexed via yamux.  Each client proxy request opens a
//! new yamux stream on a selected provider.

use anyhow::Result;
use clap::Parser;
use std::sync::Arc;
use tracing::info;

mod config;
mod http_connect;
mod pool;
mod provider;
mod socks5;

use config::Config;
use pool::ProviderPool;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("rproxy_server=info")),
        )
        .init();

    let cfg = Config::parse();
    info!("starting rproxy-server");
    info!("  socks5   = 0.0.0.0:{}", cfg.socks5_port);
    info!("  http     = 0.0.0.0:{}", cfg.http_port);
    info!("  provider = 0.0.0.0:{}", cfg.provider_port);

    let pool = Arc::new(ProviderPool::new());

    // Spawn all three listeners concurrently.
    let socks5_handle = {
        let pool = pool.clone();
        let addr = format!("0.0.0.0:{}", cfg.socks5_port);
        tokio::spawn(async move { socks5::serve(addr, pool).await })
    };

    let http_handle = {
        let pool = pool.clone();
        let addr = format!("0.0.0.0:{}", cfg.http_port);
        tokio::spawn(async move { http_connect::serve(addr, pool).await })
    };

    let provider_handle = {
        let pool = pool.clone();
        let addr = format!("0.0.0.0:{}", cfg.provider_port);
        let token = cfg.provider_token.clone();
        tokio::spawn(async move { provider::serve(addr, token, pool).await })
    };

    tokio::select! {
        r = socks5_handle   => { r??; }
        r = http_handle     => { r??; }
        r = provider_handle => { r??; }
    }

    Ok(())
}
