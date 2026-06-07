use std::fmt;
use std::time::Instant;

use futures_executor::block_on;
use futures_util::stream::SelectAll;
use slotmap::SlotMap;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::{Client, Payload, Result};

slotmap::new_key_type! {
    pub struct KernelId;
}

impl fmt::Display for KernelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

/// A kernel slot: either still booting in the background, or connected and ready.
enum KernelEntry {
    /// `Client::start` is running on a background task; not yet usable. Carries
    /// the kernelspec name and start time for the status-line progress indicator.
    Starting {
        name: String,
        since: Instant,
    },
    // Boxed because `Client` is much larger than the `Starting` variant.
    Ready(Box<Client>),
}

/// The result of a background kernel start, delivered over [`Registry::start_rx`]
/// and applied by [`Registry::finish_start`]. The `String` is the kernelspec name
/// (kept so failures can be reported even when `Client::start` returns an error).
pub type KernelStart = (
    KernelId,
    String,
    Result<(Client, UnboundedReceiver<(KernelId, Payload)>)>,
);

/// Outcome of applying a [`KernelStart`], returned by [`Registry::finish_start`].
pub enum StartOutcome {
    /// The kernel connected and is now `Ready`.
    Started { id: KernelId, name: String },
    /// The kernel failed to start; the slot has been removed.
    Failed {
        id: KernelId,
        name: String,
        error: String,
    },
    /// The slot was removed (e.g. `:jupyter-stop`) before the start finished; the
    /// freshly-started client was dropped. No user-facing error.
    Cancelled { id: KernelId },
}

/// Owns and manages the lifecycle of all running Jupyter kernels, mirroring
/// `helix_dap::registry::Registry`.
///
/// Kernel selection is per-document rather than global: a document points at a
/// `KernelId` and we look it up here. (Unlike DAP, which tracks a single active
/// client.)
pub struct Registry {
    inner: SlotMap<KernelId, KernelEntry>,
    /// Merged stream of incoming messages from every running kernel.
    pub incoming: SelectAll<UnboundedReceiverStream<(KernelId, Payload)>>,
    /// Background `start_client` tasks send their completed `Client` here; the
    /// editor event loop drains it and calls [`Registry::finish_start`].
    start_tx: UnboundedSender<KernelStart>,
    pub start_rx: UnboundedReceiver<KernelStart>,
}

impl Registry {
    pub fn new() -> Self {
        let (start_tx, start_rx) = unbounded_channel();
        Self {
            inner: SlotMap::with_key(),
            incoming: SelectAll::new(),
            start_tx,
            start_rx,
        }
    }

    /// Begin starting the kernelspec named `kernel_name` on a background task and
    /// return its `KernelId` immediately, without blocking the editor. The slot is
    /// `Starting` until the completed client arrives on `start_rx` and
    /// [`finish_start`](Self::finish_start) promotes it to `Ready`.
    ///
    /// The returned [`oneshot::Receiver`] fires once the start finishes (success
    /// or failure), so callers can stop a progress-spinner redraw ticker promptly.
    pub fn start_client(&mut self, kernel_name: &str) -> (KernelId, oneshot::Receiver<()>) {
        let name = kernel_name.to_string();
        let id = self.inner.insert(KernelEntry::Starting {
            name: name.clone(),
            since: Instant::now(),
        });
        let tx = self.start_tx.clone();
        let (done_tx, done_rx) = oneshot::channel();
        tokio::spawn(async move {
            let result = Client::start(id, &name).await;
            let _ = tx.send((id, name, result));
            let _ = done_tx.send(());
        });
        (id, done_rx)
    }

    /// Apply a completed background start. On success the slot becomes `Ready` and
    /// the kernel's message receiver is merged into `incoming`.
    pub fn finish_start(&mut self, start: KernelStart) -> StartOutcome {
        let (id, name, result) = start;
        match result {
            Ok((client, receiver)) => {
                if self.inner.contains_key(id) {
                    self.inner[id] = KernelEntry::Ready(Box::new(client));
                    self.incoming.push(UnboundedReceiverStream::new(receiver));
                    StartOutcome::Started { id, name }
                } else {
                    // The slot was removed while the kernel was booting; dropping
                    // `client` here kills the just-spawned process.
                    StartOutcome::Cancelled { id }
                }
            }
            Err(err) => {
                self.inner.remove(id);
                StartOutcome::Failed {
                    id,
                    name,
                    error: err.to_string(),
                }
            }
        }
    }

    /// Synchronously discover, spawn, connect to, and handshake the kernelspec
    /// named `kernel_name`, blocking until it is `Ready`. Intended for tests; the
    /// editor uses the non-blocking [`start_client`](Self::start_client).
    pub fn start_client_blocking(&mut self, kernel_name: &str) -> Result<KernelId> {
        self.inner.try_insert_with_key(|id| {
            let (client, receiver) = block_on(Client::start(id, kernel_name))?;
            self.incoming.push(UnboundedReceiverStream::new(receiver));
            Ok(KernelEntry::Ready(Box::new(client)))
        })
    }

    /// Remove a kernel, dropping the `Client` (which kills the process). Works on
    /// a still-`Starting` slot too (the in-flight start is then `Cancelled`).
    pub fn remove_client(&mut self, id: KernelId) {
        self.inner.remove(id);
    }

    pub fn get_client(&self, id: KernelId) -> Option<&Client> {
        match self.inner.get(id) {
            Some(KernelEntry::Ready(client)) => Some(client.as_ref()),
            _ => None,
        }
    }

    pub fn get_client_mut(&mut self, id: KernelId) -> Option<&mut Client> {
        match self.inner.get_mut(id) {
            Some(KernelEntry::Ready(client)) => Some(client.as_mut()),
            _ => None,
        }
    }

    /// Whether the slot is a kernel that is still booting.
    pub fn is_starting(&self, id: KernelId) -> bool {
        matches!(self.inner.get(id), Some(KernelEntry::Starting { .. }))
    }

    /// If the slot is `Starting`, return its kernelspec name and start time (for
    /// the progress-spinner indicator).
    pub fn starting(&self, id: KernelId) -> Option<(&str, Instant)> {
        match self.inner.get(id) {
            Some(KernelEntry::Starting { name, since }) => Some((name.as_str(), *since)),
            _ => None,
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (KernelId, &Client)> {
        self.inner.iter().filter_map(|(id, entry)| match entry {
            KernelEntry::Ready(client) => Some((id, client.as_ref())),
            KernelEntry::Starting { .. } => None,
        })
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}
