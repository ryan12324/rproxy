//! HTTP CONNECT proxy listener (RFC 7231 §4.3.6).

use std::sync::Arc;

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::compat::FuturesAsyncReadCompatExt;

use rproxy_proto::{ProxyAddr, read_proxy_response, write_proxy_request};

use crate::pool::ProviderPool;

pub async fn serve(addr: String, pool: Arc<ProviderPool>) -> Result<()> {
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("http-connect listener on {}", addr);
    loop {
        let (stream, _peer) = listener.accept().await?;
        let pool = pool.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, pool).await {
                tracing::debug!("http error: {:#}", e);
            }
        });
    }
}

async fn handle_client(stream: TcpStream, pool: Arc<ProviderPool>) -> Result<()> {
    stream.set_nodelay(true)?;
    let mut reader = BufReader::new(stream);

    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;
    let request_line = request_line.trim_end().to_string();

    // Consume headers.
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        if line == "\r\n" || line == "\n" || line.is_empty() {
            break;
        }
    }

    let parts: Vec<&str> = request_line.splitn(3, ' ').collect();
    if parts.len() != 3 {
        let mut s = reader.into_inner();
        s.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await.ok();
        anyhow::bail!("malformed request");
    }

    if parts[0] != "CONNECT" {
        let mut s = reader.into_inner();
        s.write_all(b"HTTP/1.1 405 Method Not Allowed\r\n\r\n")
            .await
            .ok();
        anyhow::bail!("unsupported method: {}", parts[0]);
    }

    let target = parse_connect_target(parts[1])?;
    tracing::debug!(%target, "HTTP CONNECT");

    let mut stream = reader.into_inner();

    let provider = match pool.pick() {
        Ok(p) => p,
        Err(e) => {
            stream
                .write_all(b"HTTP/1.1 503 Service Unavailable\r\n\r\n")
                .await
                .ok();
            return Err(e);
        }
    };

    let yamux_stream = match provider.open_stream().await {
        Ok(s) => s,
        Err(e) => {
            stream
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
                .await
                .ok();
            return Err(e);
        }
    };

    let mut tunnel = yamux_stream.compat();

    write_proxy_request(&mut tunnel, &target).await?;

    if let Err(e) = read_proxy_response(&mut tunnel).await {
        stream
            .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
            .await
            .ok();
        provider.stream_done();
        return Err(e);
    }

    stream
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;

    let result = tokio::io::copy_bidirectional(&mut stream, &mut tunnel).await;
    provider.stream_done();
    result?;
    Ok(())
}

fn parse_connect_target(s: &str) -> Result<ProxyAddr> {
    if let Some(bracket_end) = s.find("]:") {
        let host = &s[1..bracket_end];
        let port_str = &s[bracket_end + 2..];
        let port: u16 = port_str
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid port: {}", port_str))?;
        let addr: std::net::Ipv6Addr = host
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid IPv6: {}", host))?;
        Ok(ProxyAddr::Ipv6(addr, port))
    } else {
        let (host, port_str) = s
            .rsplit_once(':')
            .ok_or_else(|| anyhow::anyhow!("missing port in: {}", s))?;
        let port: u16 = port_str
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid port: {}", port_str))?;
        if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
            Ok(ProxyAddr::Ipv4(ip, port))
        } else {
            Ok(ProxyAddr::Domain(host.to_string(), port))
        }
    }
}
