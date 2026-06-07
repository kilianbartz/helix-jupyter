//! Jupyter kernel client for Helix.
//!
//! Structured after `helix-dap`: a [`Client`] wraps a single running kernel, a
//! [`registry::Registry`] owns all kernels and exposes a merged stream of
//! incoming messages, and [`Payload`] is the editor-facing message type. The
//! ZeroMQ wire protocol, multipart framing and HMAC signing are handled by the
//! `jupyter-zmq-client` crate; the Jupyter message types come from
//! `jupyter-protocol`.

mod client;
mod messages;
pub mod registry;

pub use client::Client;
pub use messages::{media_to_text, Channel, Payload};
pub use registry::{KernelId, Registry};

// Re-export the protocol types callers need to inspect messages.
pub use jupyter_protocol::{
    ExecuteResult, ExecutionState, JupyterMessage, JupyterMessageContent, Media, MediaType, Stdio,
    StreamContent,
};

use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("request timed out")]
    Timeout,
    #[error("the kernel connection was closed")]
    StreamClosed,
    #[error(transparent)]
    Runtime(#[from] jupyter_zmq_client::RuntimeError),
    #[error(transparent)]
    Serde(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = core::result::Result<T, Error>;

/// A discovered kernelspec, for the kernel-selection picker.
#[derive(Debug, Clone)]
pub struct KernelSpec {
    /// The kernelspec name passed to [`Registry::start_client`].
    pub name: String,
    pub display_name: String,
    pub language: String,
}

/// List installed kernelspecs (including virtualenv kernels reported by
/// `jupyter --paths`). Blocks on async discovery; call from within a tokio
/// runtime context.
pub fn available_kernels() -> Vec<KernelSpec> {
    futures_executor::block_on(jupyter_zmq_client::list_kernelspecs_with_jupyter_paths())
        .into_iter()
        .map(|spec| KernelSpec {
            name: spec.kernel_name,
            display_name: spec.kernelspec.display_name,
            language: spec.kernelspec.language,
        })
        .collect()
}
