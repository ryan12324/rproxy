//! WebSocket tunnel + yamux stream handling for the agent.

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async_with_config, tungstenite::protocol::WebSocketConfig};
use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};
use tungstenite::Message;
use yamux::{Config as YamuxConfig, Connection, Mode, Stream};

use rproxy_proto::{CtrlMsg, WsBinary, read_proxy_request, write_proxy_response};

use crate::config::Config;

pub async fn run(cfg: &Config) -> Result<()> {
    tracing::info!("connecting to {}", cfg.server_url);

    let ws_config = WebSocketConfig {
        max_message_size: Some(64 * 1024 * 1024),
        max_frame_size: Some(16 * 1024 * 1024),
        ..Default::default()
    };

    let (mut ws, _response) =
        connect_async_with_config(&cfg.server_url, Some(ws_config), cfg.accept_invalid_certs)
            .await
            .map_err(|e| anyhow::anyhow!("WebSocket connect failed: {}", e))?;

    tracing::info!("connected; authenticating");

    // Register with server.
    let reg = CtrlMsg::Register {
        token: cfg.token.clone(),
        geo: cfg.geo.clone(),
        label: cfg.label.clone(),
    };
    ws.send(Message::Text(serde_json::to_string(&reg)?.into())).await?;

    let response = match ws.next().await {
        Some(Ok(Message::Text(txt))) => serde_json::from_str::<CtrlMsg>(&txt)?,
        Some(Ok(Message::Binary(b))) => serde_json::from_slice::<CtrlMsg>(&b)?,
        other => anyhow::bail!("unexpected auth response: {:?}", other),
    };

    match response {
        CtrlMsg::Registered { provider_id } => {
            tracing::info!(%provider_id, "registered with server");
        }
        CtrlMsg::Rejected { reason } => {
            anyhow::bail!("registration rejected: {}", reason);
        }
        other => anyhow::bail!("unexpected response: {:?}", other),
    }

    // Agent is yamux Server: accepts streams opened by the gateway.
    let ws_binary = WsBinary::new(ws);
    let compat = ws_binary.compat();
    let mut conn = Connection::new(compat, YamuxConfig::default(), Mode::Server);

    tracing::info!("yamux ready; waiting for proxy streams");

    // Accept streams until the connection closes.
    loop {
        match std::future::poll_fn(|cx| conn.poll_next_inbound(cx)).await {
            Some(Ok(stream)) => {
                tokio::spawn(async move {
                    if let Err(e) = handle_stream(stream).await {
                        tracing::debug!("stream error: {:#}", e);
                    }
                });
            }
            Some(Err(e)) => {
                return Err(anyhow::anyhow!("yamux error: {}", e));
            }
            None => {
                tracing::info!("yamux connection closed");
                return Ok(());
            }
        }
    }
}

/// Handle one proxy stream: read header → connect to target → relay.
async fn handle_stream(stream: Stream) -> Result<()> {
    // Bridge yamux Stream (futures AsyncRead/AsyncWrite) to tokio.
    let mut s = stream.compat();

    let target = read_proxy_request(&mut s)
        .await
        .map_err(|e| anyhow::anyhow!("read proxy request: {}", e))?;

    tracing::debug!(%target, "proxy stream");

    let addrs = target.to_socket_addrs().await?;
    let mut tcp = None;
    let mut last_err = None;
    for addr in addrs {
        match TcpStream::connect(addr).await {
            Ok(s) => {
                tcp = Some(s);
                break;
            }
            Err(e) => last_err = Some(e),
        }
    }

    let mut tcp = match tcp {
        Some(t) => t,
        None => {
            let e = last_err
                .map(|e| anyhow::anyhow!("{}", e))
                .unwrap_or_else(|| anyhow::anyhow!("no addresses for {}", target));
            write_proxy_response(&mut s, Err(anyhow::anyhow!("{}", e))).await.ok();
            return Err(e);
        }
    };

    tcp.set_nodelay(true).ok();
    tracing::debug!(%target, "target connected");

    write_proxy_response(&mut s, Ok(())).await?;
    tokio::io::copy_bidirectional(&mut tcp, &mut s).await?;
    Ok(())
}
