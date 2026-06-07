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
pub use messages::{media_to_png, media_to_text, Channel, Payload};
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

/// If a Python virtual environment is currently activated (the `VIRTUAL_ENV`
/// environment variable is set) and it ships the `jupyter` CLI, return the name
/// of an installed kernelspec whose interpreter lives inside that venv.
///
/// This lets `:jupyter-start` default to the active environment's kernel rather
/// than (or in absence of) the configured `default-kernel`. Returns `None` when
/// no venv is active, the venv has no `jupyter` installed, or no kernel resolves
/// to it. Blocks on async discovery; call from within a tokio runtime context.
pub fn active_venv_kernel() -> Option<String> {
    use std::path::{Path, PathBuf};

    let venv = PathBuf::from(std::env::var_os("VIRTUAL_ENV")?);

    // Only treat the venv as a Jupyter environment if it actually ships the
    // `jupyter` CLI, i.e. jupyter is installed in it.
    fn venv_has_jupyter(venv: &Path) -> bool {
        let candidates = if cfg!(windows) {
            vec![
                venv.join("Scripts").join("jupyter.exe"),
                venv.join("Scripts").join("jupyter"),
            ]
        } else {
            vec![venv.join("bin").join("jupyter")]
        };
        candidates.iter().any(|p| p.exists())
    }
    if !venv_has_jupyter(&venv) {
        return None;
    }

    // Resolve symlinks so the prefix check works even when the venv path is
    // itself a symlink (or paths are symlinked into the venv).
    let venv = std::fs::canonicalize(&venv).unwrap_or(venv);
    let under_venv = |path: &Path| {
        let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        resolved.starts_with(&venv)
    };

    futures_executor::block_on(jupyter_zmq_client::list_kernelspecs_with_jupyter_paths())
        .into_iter()
        .find(|spec| {
            // A kernel belongs to the venv if its kernelspec directory lives
            // inside the venv (e.g. ipykernel's built-in `python3` spec under
            // `$VIRTUAL_ENV/share/jupyter`, whose argv is just `"python"`), or
            // if its interpreter (`argv[0]`) is an absolute path into the venv
            // (e.g. a `--user`-installed spec pinned to the venv's Python).
            under_venv(&spec.path)
                || spec
                    .kernelspec
                    .argv
                    .first()
                    .is_some_and(|interp| under_venv(Path::new(interp)))
        })
        .map(|spec| spec.kernel_name)
}
