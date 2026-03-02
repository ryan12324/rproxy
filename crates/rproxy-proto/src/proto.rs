//! Wire protocol: framing inside yamux streams.
//!
//! ## Control channel (stream 1 on every tunnel)
//! Bidirectional JSON newline-delimited messages used for auth and heartbeat.
//!
//! ## Proxy streams (all other streams)
//! Each yamux stream carries exactly one proxied TCP connection.
//!
//! ```text
//! ┌─────────┬──────────────────┬──────────┐
//! │ Ver(1B) │ AddrType(1B)     │ Port(2B) │
//! │ Addr…   │ (variable)       │          │
//! └─────────┴──────────────────┴──────────┘
//! ```
//! Followed immediately by the raw TCP byte stream.
//!
//! The agent writes a 1-byte status response before starting relay:
//!   0x00 = success, 0x01 = error (followed by 1-byte length + message)

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use anyhow::{bail, Result};
use bytes::{BufMut, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const PROTO_VERSION: u8 = 0x01;

pub const ATYP_IPV4: u8 = 0x01;
pub const ATYP_DOMAIN: u8 = 0x03;
pub const ATYP_IPV6: u8 = 0x04;

pub const STATUS_OK: u8 = 0x00;
pub const STATUS_ERR: u8 = 0x01;

/// Target address for a proxied connection.
#[derive(Debug, Clone)]
pub enum ProxyAddr {
    Ipv4(Ipv4Addr, u16),
    Ipv6(Ipv6Addr, u16),
    Domain(String, u16),
}

impl ProxyAddr {
    pub fn port(&self) -> u16 {
        match self {
            Self::Ipv4(_, p) | Self::Ipv6(_, p) | Self::Domain(_, p) => *p,
        }
    }

    pub async fn to_socket_addrs(&self) -> io::Result<Vec<SocketAddr>> {
        match self {
            Self::Ipv4(ip, port) => Ok(vec![SocketAddr::new(IpAddr::V4(*ip), *port)]),
            Self::Ipv6(ip, port) => Ok(vec![SocketAddr::new(IpAddr::V6(*ip), *port)]),
            Self::Domain(host, port) => {
                let addrs = tokio::net::lookup_host(format!("{}:{}", host, port))
                    .await?
                    .collect();
                Ok(addrs)
            }
        }
    }
}

impl std::fmt::Display for ProxyAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ipv4(ip, port) => write!(f, "{}:{}", ip, port),
            Self::Ipv6(ip, port) => write!(f, "[{}]:{}", ip, port),
            Self::Domain(host, port) => write!(f, "{}:{}", host, port),
        }
    }
}

/// Write a proxy request header to an async writer.
pub async fn write_proxy_request<W: AsyncWrite + Unpin>(
    w: &mut W,
    addr: &ProxyAddr,
) -> Result<()> {
    let mut buf = BytesMut::with_capacity(32);
    buf.put_u8(PROTO_VERSION);

    match addr {
        ProxyAddr::Ipv4(ip, port) => {
            buf.put_u8(ATYP_IPV4);
            buf.put_slice(&ip.octets());
            buf.put_u16(*port);
        }
        ProxyAddr::Ipv6(ip, port) => {
            buf.put_u8(ATYP_IPV6);
            buf.put_slice(&ip.octets());
            buf.put_u16(*port);
        }
        ProxyAddr::Domain(host, port) => {
            let host_bytes = host.as_bytes();
            if host_bytes.len() > 255 {
                bail!("domain too long: {}", host);
            }
            buf.put_u8(ATYP_DOMAIN);
            buf.put_u8(host_bytes.len() as u8);
            buf.put_slice(host_bytes);
            buf.put_u16(*port);
        }
    }

    w.write_all(&buf).await?;
    Ok(())
}

/// Read a proxy request header from an async reader.
pub async fn read_proxy_request<R: AsyncRead + Unpin>(r: &mut R) -> Result<ProxyAddr> {
    let version = r.read_u8().await?;
    if version != PROTO_VERSION {
        bail!("unsupported protocol version: {:#x}", version);
    }

    let atyp = r.read_u8().await?;
    let addr = match atyp {
        ATYP_IPV4 => {
            let mut octets = [0u8; 4];
            r.read_exact(&mut octets).await?;
            let port = r.read_u16().await?;
            ProxyAddr::Ipv4(Ipv4Addr::from(octets), port)
        }
        ATYP_IPV6 => {
            let mut octets = [0u8; 16];
            r.read_exact(&mut octets).await?;
            let port = r.read_u16().await?;
            ProxyAddr::Ipv6(Ipv6Addr::from(octets), port)
        }
        ATYP_DOMAIN => {
            let len = r.read_u8().await? as usize;
            let mut host_bytes = vec![0u8; len];
            r.read_exact(&mut host_bytes).await?;
            let host = String::from_utf8(host_bytes)
                .map_err(|e| anyhow::anyhow!("invalid domain: {}", e))?;
            let port = r.read_u16().await?;
            ProxyAddr::Domain(host, port)
        }
        other => bail!("unknown address type: {:#x}", other),
    };

    Ok(addr)
}

/// Write the agent's response to a proxy request.
pub async fn write_proxy_response<W: AsyncWrite + Unpin>(
    w: &mut W,
    result: Result<()>,
) -> Result<()> {
    match result {
        Ok(()) => {
            w.write_u8(STATUS_OK).await?;
        }
        Err(e) => {
            let msg = e.to_string();
            let msg_bytes = msg.as_bytes();
            let len = msg_bytes.len().min(255) as u8;
            w.write_u8(STATUS_ERR).await?;
            w.write_u8(len).await?;
            w.write_all(&msg_bytes[..len as usize]).await?;
        }
    }
    Ok(())
}

/// Read the agent's response to a proxy request.
pub async fn read_proxy_response<R: AsyncRead + Unpin>(r: &mut R) -> Result<()> {
    let status = r.read_u8().await?;
    match status {
        STATUS_OK => Ok(()),
        STATUS_ERR => {
            let len = r.read_u8().await? as usize;
            let mut msg = vec![0u8; len];
            r.read_exact(&mut msg).await?;
            let msg = String::from_utf8_lossy(&msg).into_owned();
            bail!("agent error: {}", msg)
        }
        other => bail!("unknown response status: {:#x}", other),
    }
}

/// Provider authentication message (JSON, newline-terminated, sent before yamux).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CtrlMsg {
    /// Agent → Server: register with auth token.
    Register {
        token: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        geo: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Server → Agent: registration accepted.
    Registered { provider_id: String },
    /// Server → Agent: registration rejected.
    Rejected { reason: String },
}
