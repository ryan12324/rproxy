//! SOCKS5 proxy listener (RFC 1928, no-auth, TCP CONNECT only).

use std::io;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::compat::FuturesAsyncReadCompatExt;

use rproxy_proto::{ProxyAddr, read_proxy_response, write_proxy_request};

use crate::pool::ProviderPool;

const SOCKS5_VERSION: u8 = 0x05;
const NO_AUTH: u8 = 0x00;
const CMD_CONNECT: u8 = 0x01;
const ATYP_IPV4: u8 = 0x01;
const ATYP_DOMAIN: u8 = 0x03;
const ATYP_IPV6: u8 = 0x04;
const REP_SUCCESS: u8 = 0x00;
const REP_GENERAL_FAILURE: u8 = 0x01;
const REP_CMD_NOT_SUPPORTED: u8 = 0x07;

pub async fn serve(addr: String, pool: Arc<ProviderPool>) -> Result<()> {
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("socks5 listener on {}", addr);
    loop {
        let (stream, peer) = listener.accept().await?;
        tracing::debug!(%peer, "socks5 client");
        let pool = pool.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, pool).await {
                tracing::debug!("socks5 error: {:#}", e);
            }
        });
    }
}

async fn handle_client(mut stream: TcpStream, pool: Arc<ProviderPool>) -> Result<()> {
    stream.set_nodelay(true)?;

    // Greeting
    let version = stream.read_u8().await?;
    if version != SOCKS5_VERSION {
        anyhow::bail!("unsupported SOCKS version: {}", version);
    }
    let nmethods = stream.read_u8().await? as usize;
    let mut methods = vec![0u8; nmethods];
    stream.read_exact(&mut methods).await?;
    stream.write_all(&[SOCKS5_VERSION, NO_AUTH]).await?;

    // Request
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).await?;
    let [ver, cmd, _rsv, atyp] = header;
    if ver != SOCKS5_VERSION {
        anyhow::bail!("bad version in request: {}", ver);
    }
    if cmd != CMD_CONNECT {
        send_socks5_reply(&mut stream, REP_CMD_NOT_SUPPORTED).await.ok();
        anyhow::bail!("unsupported SOCKS5 command: {}", cmd);
    }

    let target = match atyp {
        ATYP_IPV4 => {
            let mut buf = [0u8; 4];
            stream.read_exact(&mut buf).await?;
            ProxyAddr::Ipv4(Ipv4Addr::from(buf), stream.read_u16().await?)
        }
        ATYP_IPV6 => {
            let mut buf = [0u8; 16];
            stream.read_exact(&mut buf).await?;
            ProxyAddr::Ipv6(Ipv6Addr::from(buf), stream.read_u16().await?)
        }
        ATYP_DOMAIN => {
            let len = stream.read_u8().await? as usize;
            let mut host = vec![0u8; len];
            stream.read_exact(&mut host).await?;
            let port = stream.read_u16().await?;
            ProxyAddr::Domain(
                String::from_utf8(host).map_err(|_| anyhow::anyhow!("invalid domain"))?,
                port,
            )
        }
        _ => {
            send_socks5_reply(&mut stream, REP_GENERAL_FAILURE).await.ok();
            anyhow::bail!("unsupported atyp: {}", atyp);
        }
    };

    tracing::debug!(%target, "SOCKS5 CONNECT");

    let provider = match pool.pick() {
        Ok(p) => p,
        Err(e) => {
            send_socks5_reply(&mut stream, REP_GENERAL_FAILURE).await.ok();
            return Err(e);
        }
    };

    let yamux_stream = match provider.open_stream().await {
        Ok(s) => s,
        Err(e) => {
            send_socks5_reply(&mut stream, REP_GENERAL_FAILURE).await.ok();
            return Err(e);
        }
    };

    let mut tunnel = yamux_stream.compat();
    write_proxy_request(&mut tunnel, &target).await?;

    if let Err(e) = read_proxy_response(&mut tunnel).await {
        send_socks5_reply(&mut stream, REP_GENERAL_FAILURE).await.ok();
        provider.stream_done();
        return Err(e);
    }

    send_socks5_reply(&mut stream, REP_SUCCESS).await?;

    let result = tokio::io::copy_bidirectional(&mut stream, &mut tunnel).await;
    provider.stream_done();
    result?;
    Ok(())
}

async fn send_socks5_reply<W: AsyncWrite + Unpin>(w: &mut W, rep: u8) -> io::Result<()> {
    w.write_all(&[SOCKS5_VERSION, rep, 0x00, ATYP_IPV4, 0, 0, 0, 0, 0, 0])
        .await
}
