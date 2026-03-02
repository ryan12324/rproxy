//! WebSocket listener for provider agent connections.

use std::collections::VecDeque;
use std::sync::Arc;
use std::task::Poll;

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::accept_async;
use tokio_util::compat::TokioAsyncReadCompatExt;
use tungstenite::Message;
use uuid::Uuid;
use yamux::{Config as YamuxConfig, Connection, Mode};

use rproxy_proto::{CtrlMsg, WsBinary};

use crate::pool::{OpenReply, ProviderHandle, ProviderPool};

pub async fn serve(addr: String, token: String, pool: Arc<ProviderPool>) -> Result<()> {
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("provider listener on {}", addr);

    loop {
        let (stream, peer) = listener.accept().await?;
        tracing::debug!(%peer, "provider tcp connection");
        let pool = pool.clone();
        let token = token.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_provider(stream, token, pool).await {
                tracing::warn!("provider error: {:#}", e);
            }
        });
    }
}

async fn handle_provider(stream: TcpStream, token: String, pool: Arc<ProviderPool>) -> Result<()> {
    stream.set_nodelay(true)?;
    let mut ws = accept_async(stream).await?;

    // --- Authentication handshake ---
    let reg_msg = match ws.next().await {
        Some(Ok(Message::Text(txt))) => serde_json::from_str::<CtrlMsg>(&txt)?,
        Some(Ok(Message::Binary(b))) => serde_json::from_slice::<CtrlMsg>(&b)?,
        other => anyhow::bail!("expected register message, got {:?}", other),
    };

    let (provider_token, geo, label) = match reg_msg {
        CtrlMsg::Register { token: t, geo, label } => (t, geo, label),
        other => anyhow::bail!("expected Register, got {:?}", other),
    };

    if provider_token != token {
        let reject = serde_json::to_string(&CtrlMsg::Rejected {
            reason: "invalid token".into(),
        })?;
        ws.send(Message::Text(reject.into())).await.ok();
        anyhow::bail!("invalid provider token");
    }

    let provider_id = Uuid::new_v4();
    let registered = serde_json::to_string(&CtrlMsg::Registered {
        provider_id: provider_id.to_string(),
    })?;
    ws.send(Message::Text(registered.into())).await?;
    tracing::info!(%provider_id, ?geo, ?label, "provider authenticated");

    // --- Yamux setup (gateway = Mode::Client: opens streams) ---
    let ws_binary = WsBinary::new(ws);
    let compat = ws_binary.compat();
    let conn = Connection::new(compat, YamuxConfig::default(), Mode::Client);

    // Channel for proxy request tasks to request new yamux streams.
    let (open_tx, open_rx) = tokio::sync::mpsc::unbounded_channel::<OpenReply>();
    let handle = ProviderHandle::new(provider_id, label, geo, open_tx);
    pool.insert(Arc::clone(&handle));

    // Drive the connection until it closes.
    drive_connection(conn, open_rx).await;
    pool.remove(&provider_id);
    Ok(())
}

/// Drive a gateway yamux Connection (Mode::Client).
///
/// Uses a single `poll_fn` loop that:
///  - Receives open-stream requests from proxy tasks via `open_rx`
///  - Fulfills them via `poll_new_outbound`
///  - Drives inbound via `poll_next_inbound` (processes ACKs / window updates)
async fn drive_connection<T>(
    mut conn: Connection<T>,
    mut open_rx: tokio::sync::mpsc::UnboundedReceiver<OpenReply>,
) where
    T: futures_util::AsyncRead + futures_util::AsyncWrite + Unpin,
{
    let mut pending: VecDeque<OpenReply> = VecDeque::new();

    loop {
        let alive = std::future::poll_fn(|cx| {
            // 1. Drain new open-stream requests (non-blocking).
            loop {
                match open_rx.poll_recv(cx) {
                    Poll::Ready(Some(tx)) => pending.push_back(tx),
                    Poll::Ready(None) => return Poll::Ready(false), // channel closed
                    Poll::Pending => break,
                }
            }

            // 2. Fulfill pending open requests.
            while !pending.is_empty() {
                match conn.poll_new_outbound(cx) {
                    Poll::Ready(Ok(s)) => {
                        pending.pop_front().unwrap().send(Ok(s)).ok();
                    }
                    Poll::Ready(Err(e)) => {
                        let tx = pending.pop_front().unwrap();
                        tx.send(Err(anyhow::anyhow!("yamux outbound: {}", e))).ok();
                        return Poll::Ready(false);
                    }
                    Poll::Pending => break,
                }
            }

            // 3. Drive inbound frames (ACKs, window updates, GO_AWAY).
            loop {
                match conn.poll_next_inbound(cx) {
                    Poll::Ready(Some(Ok(_stream))) => {
                        // Unexpected inbound stream in client mode — ignore.
                    }
                    Poll::Ready(Some(Err(e))) => {
                        tracing::debug!("yamux inbound error: {}", e);
                        return Poll::Ready(false);
                    }
                    Poll::Ready(None) => return Poll::Ready(false),
                    Poll::Pending => break,
                }
            }

            Poll::Pending
        })
        .await;

        if !alive {
            break;
        }
    }

    // Drain pending requests with an error.
    while let Some(tx) = pending.pop_front() {
        tx.send(Err(anyhow::anyhow!("provider connection closed"))).ok();
    }
}
