use std::fmt;

use futures_executor::block_on;
use futures_util::stream::SelectAll;
use slotmap::SlotMap;
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

/// Owns and manages the lifecycle of all running Jupyter kernels, mirroring
/// `helix_dap::registry::Registry`.
///
/// Kernel selection is per-document rather than global: a document points at a
/// `KernelId` and we look it up here. (Unlike DAP, which tracks a single active
/// client.)
pub struct Registry {
    inner: SlotMap<KernelId, Client>,
    /// Merged stream of incoming messages from every running kernel.
    pub incoming: SelectAll<UnboundedReceiverStream<(KernelId, Payload)>>,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            inner: SlotMap::with_key(),
            incoming: SelectAll::new(),
        }
    }

    /// Discover, spawn, and connect to the kernelspec named `kernel_name`.
    pub fn start_client(&mut self, kernel_name: &str) -> Result<KernelId> {
        self.inner.try_insert_with_key(|id| {
            let (client, receiver) = block_on(Client::start(id, kernel_name))?;
            self.incoming.push(UnboundedReceiverStream::new(receiver));
            Ok(client)
        })
    }

    /// Remove a kernel, dropping the `Client` (which kills the process).
    pub fn remove_client(&mut self, id: KernelId) {
        self.inner.remove(id);
    }

    pub fn get_client(&self, id: KernelId) -> Option<&Client> {
        self.inner.get(id)
    }

    pub fn get_client_mut(&mut self, id: KernelId) -> Option<&mut Client> {
        self.inner.get_mut(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (KernelId, &Client)> {
        self.inner.iter()
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}
