use jupyter_protocol::{JupyterMessage, MediaType};

/// Which Jupyter channel a message should be sent on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Shell,
    Control,
    Stdin,
}

/// A message coming *from* the kernel, tagged with the channel it arrived on.
///
/// This is the analog of `helix_dap::Payload` — the registry merges a stream of
/// `(KernelId, Payload)` from every running kernel, and the editor dispatches on
/// it. Results of an execution are correlated back to the originating request via
/// `JupyterMessage::parent_header`'s `msg_id`.
#[derive(Debug)]
pub enum Payload {
    /// A reply on the shell channel (e.g. `execute_reply`, `kernel_info_reply`).
    Shell(JupyterMessage),
    /// A broadcast on the iopub channel (`stream`, `execute_result`,
    /// `display_data`, `status`, `error`, ...).
    IoPub(JupyterMessage),
    /// A reply on the control channel (`interrupt_reply`, `shutdown_reply`).
    Control(JupyterMessage),
    /// An `input_request` from the kernel on the stdin channel.
    Stdin(JupyterMessage),
}

/// Extract a plain-text representation from a Jupyter MIME bundle for display in
/// the terminal UI. Prefers `text/plain`, falling back to other textual types.
pub fn media_to_text(media: &jupyter_protocol::Media) -> Option<String> {
    let richest = media.richest(|mime| match mime {
        MediaType::Plain(_) => 4,
        MediaType::Markdown(_) => 3,
        MediaType::Latex(_) => 2,
        MediaType::Html(_) => 1,
        _ => 0,
    })?;
    match richest {
        MediaType::Plain(s) | MediaType::Markdown(s) | MediaType::Latex(s) | MediaType::Html(s) => {
            Some(s.clone())
        }
        _ => None,
    }
}
