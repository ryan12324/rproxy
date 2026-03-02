//! Provider connection pool.
//!
//! Each provider is represented by a [`ProviderHandle`] that communicates
//! with a dedicated "driver" task that owns the yamux Connection.
//! Opening a new stream sends a request over a channel; the driver task
//! fulfills it via `Connection::poll_new_outbound`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;
use dashmap::DashMap;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;
use yamux::Stream;

pub type OpenReply = oneshot::Sender<Result<Stream>>;

/// Handle to a live provider connection.
pub struct ProviderHandle {
    pub id: Uuid,
    pub label: Option<String>,
    pub geo: Option<String>,
    /// Channel to the driver task that owns the yamux Connection.
    open_tx: mpsc::UnboundedSender<OpenReply>,
}

impl ProviderHandle {
    pub fn new(
        id: Uuid,
        label: Option<String>,
        geo: Option<String>,
        open_tx: mpsc::UnboundedSender<OpenReply>,
    ) -> Arc<Self> {
        Arc::new(Self { id, label, geo, open_tx })
    }

    /// Open a new proxy stream toward this provider.
    pub async fn open_stream(&self) -> Result<Stream> {
        let (tx, rx) = oneshot::channel();
        self.open_tx
            .send(tx)
            .map_err(|_| anyhow::anyhow!("provider {} disconnected", self.id))?;
        rx.await
            .map_err(|_| anyhow::anyhow!("provider {} driver dropped reply", self.id))?
    }
}

/// Concurrent pool of live provider connections.
pub struct ProviderPool {
    providers: DashMap<Uuid, Arc<ProviderHandle>>,
    counter: AtomicUsize,
}

impl ProviderPool {
    pub fn new() -> Self {
        Self {
            providers: DashMap::new(),
            counter: AtomicUsize::new(0),
        }
    }

    pub fn insert(&self, handle: Arc<ProviderHandle>) {
        tracing::info!(
            provider_id = %handle.id,
            geo = ?handle.geo,
            label = ?handle.label,
            "provider registered"
        );
        self.providers.insert(handle.id, handle);
    }

    pub fn remove(&self, id: &Uuid) {
        if self.providers.remove(id).is_some() {
            tracing::info!(%id, "provider removed");
        }
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// Round-robin provider selection.
    pub fn pick(&self) -> Result<Arc<ProviderHandle>> {
        if self.providers.is_empty() {
            anyhow::bail!("no providers available");
        }
        let idx = self.counter.fetch_add(1, Ordering::Relaxed);
        let handles: Vec<_> = self.providers.iter().map(|e| Arc::clone(e.value())).collect();
        Ok(Arc::clone(&handles[idx % handles.len()]))
    }
}

impl Default for ProviderPool {
    fn default() -> Self {
        Self::new()
    }
}
