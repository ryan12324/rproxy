use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(name = "rproxy-server", about = "Residential proxy gateway server")]
pub struct Config {
    /// Port for SOCKS5 client connections.
    #[arg(long, env = "RPROXY_SOCKS5_PORT", default_value = "1080")]
    pub socks5_port: u16,

    /// Port for HTTP CONNECT client connections.
    #[arg(long, env = "RPROXY_HTTP_PORT", default_value = "8080")]
    pub http_port: u16,

    /// Port for provider agent WebSocket connections.
    #[arg(long, env = "RPROXY_PROVIDER_PORT", default_value = "8888")]
    pub provider_port: u16,

    /// Shared secret token that provider agents must present to authenticate.
    #[arg(long, env = "RPROXY_PROVIDER_TOKEN", default_value = "change-me-in-production")]
    pub provider_token: String,
}
