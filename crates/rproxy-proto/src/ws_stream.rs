//! WebSocket → AsyncRead + AsyncWrite bridge.
//!
//! `WsBinary<S>` wraps a `WebSocketStream<S>` and exposes it as a plain
//! byte-stream I/O type (tokio `AsyncRead` + `AsyncWrite`).

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::{Buf, Bytes};
use futures_util::{Sink, Stream};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_tungstenite::WebSocketStream;
use tungstenite::Message;

/// Wraps a [`WebSocketStream`] exposing it as `AsyncRead + AsyncWrite`.
pub struct WsBinary<S> {
    inner: WebSocketStream<S>,
    /// Buffered bytes from the current in-flight binary frame.
    read_buf: Bytes,
}

impl<S> WsBinary<S> {
    pub fn new(ws: WebSocketStream<S>) -> Self {
        Self {
            inner: ws,
            read_buf: Bytes::new(),
        }
    }
}

impl<S: Unpin> Unpin for WsBinary<S> {}

impl<S> AsyncRead for WsBinary<S>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            // Drain buffered bytes first.
            if !self.read_buf.is_empty() {
                let n = buf.remaining().min(self.read_buf.len());
                buf.put_slice(&self.read_buf[..n]);
                self.read_buf.advance(n);
                return Poll::Ready(Ok(()));
            }

            // Poll the WebSocket for the next frame.
            match Pin::new(&mut self.inner).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(Ok(())), // EOF
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, e)));
                }
                Poll::Ready(Some(Ok(msg))) => match msg {
                    Message::Binary(data) => {
                        // tungstenite 0.24 Binary is Vec<u8>
                        self.read_buf = Bytes::from(data);
                        // loop to drain
                    }
                    Message::Close(_) => return Poll::Ready(Ok(())),
                    // Ping/Pong/Text: ignored (tungstenite auto-replies to Ping)
                    _ => {}
                },
            }
        }
    }
}

impl<S> AsyncWrite for WsBinary<S>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match Pin::new(&mut self.inner).poll_ready(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(e)) => {
                return Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, e)));
            }
            Poll::Ready(Ok(())) => {}
        }

        let msg = Message::Binary(buf.to_vec());
        match Pin::new(&mut self.inner).start_send(msg) {
            Ok(()) => Poll::Ready(Ok(buf.len())),
            Err(e) => Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, e))),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner)
            .poll_flush(cx)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner)
            .poll_close(cx)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }
}
