use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(name = "rproxy-agent", about = "Residential proxy exit-node agent")]
pub struct Config {
    /// WebSocket URL of the proxy server provider endpoint.
    /// Use ws:// for plain or wss:// for TLS.
    #[arg(long, env = "RPROXY_SERVER_URL", default_value = "ws://127.0.0.1:8888")]
    pub server_url: String,

    /// Authentication token (must match server's RPROXY_PROVIDER_TOKEN).
    #[arg(long, env = "RPROXY_TOKEN", default_value = "change-me-in-production")]
    pub token: String,

    /// Optional geo label for the provider (e.g. "US-CA").
    #[arg(long, env = "RPROXY_GEO")]
    pub geo: Option<String>,

    /// Optional human-readable label for this provider instance.
    #[arg(long, env = "RPROXY_LABEL")]
    pub label: Option<String>,

    /// Accept self-signed TLS certificates (for testing only).
    #[arg(long, env = "RPROXY_ACCEPT_INVALID_CERTS")]
    pub accept_invalid_certs: bool,
}
