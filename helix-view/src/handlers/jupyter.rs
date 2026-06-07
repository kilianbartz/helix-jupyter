use helix_jupyter::{
    media_to_png, media_to_text, registry::KernelId, ExecutionState as KernelStatus,
    JupyterMessage, JupyterMessageContent, Payload, Stdio,
};

use crate::jupyter::{strip_ansi, ExecutionState, JupyterImage, JupyterOutput, OutputKind};
use crate::Editor;

impl Editor {
    /// Handle a message from a running Jupyter kernel. Returns `true` if the UI
    /// needs to be re-rendered. Mirrors [`Editor::handle_debugger_message`].
    pub async fn handle_jupyter_message(&mut self, _id: KernelId, payload: Payload) -> bool {
        match payload {
            Payload::IoPub(msg) => self.handle_iopub(msg),
            Payload::Stdin(msg) => {
                // Minimal stdin handling: reply with empty input so a kernel that
                // calls `input()` doesn't hang. Interactive prompts are Phase 3.
                if let JupyterMessageContent::InputRequest(_) = msg.content {
                    if let Some(parent) = msg.parent_header.as_ref() {
                        if let Some(kernel) = self.find_output_mut(&parent.msg_id).map(|o| o.kernel)
                        {
                            if let Some(client) = self.jupyter.get_client(kernel) {
                                let _ = client.input_reply(String::new());
                            }
                        }
                    }
                }
                false
            }
            // Shell/control replies carry no displayable content beyond iopub.
            Payload::Shell(_) | Payload::Control(_) => false,
        }
    }

    fn handle_iopub(&mut self, msg: JupyterMessage) -> bool {
        let Some(execution_id) = msg.parent_header.as_ref().map(|h| h.msg_id.clone()) else {
            return false;
        };

        // Pull any PNG out of the bundle and allocate its image id *before*
        // borrowing the target output, since both need `&mut self`. An id is
        // only wasted in the (practically impossible) case of an image arriving
        // on the silent introspection follow-up.
        let new_image = match &msg.content {
            JupyterMessageContent::DisplayData(data) => media_to_png(&data.data),
            JupyterMessageContent::ExecuteResult(result) => media_to_png(&result.data),
            _ => None,
        }
        .map(str::to_string)
        .and_then(|base64| JupyterImage::from_png_base64(self.alloc_jupyter_image_id(), base64));

        let Some((output, is_inspect)) = self.find_target(&execution_id) else {
            return false;
        };

        // The silent variable-introspection follow-up: accumulate its stdout
        // (a JSON object) and parse it once the execution goes idle.
        if is_inspect {
            return match msg.content {
                JupyterMessageContent::StreamContent(stream) => {
                    output.inspect_buffer.push_str(&stream.text);
                    false
                }
                JupyterMessageContent::ExecuteResult(result) => {
                    if let Some(text) = media_to_text(&result.data) {
                        output.inspect_buffer.push_str(&text);
                    }
                    false
                }
                JupyterMessageContent::Status(status)
                    if matches!(status.execution_state, KernelStatus::Idle) =>
                {
                    output.parse_inspect_buffer();
                    true
                }
                _ => false,
            };
        }

        match msg.content {
            JupyterMessageContent::StreamContent(stream) => {
                let kind = match stream.name {
                    Stdio::Stdout => OutputKind::Stdout,
                    Stdio::Stderr => OutputKind::Stderr,
                };
                output.push_text(&stream.text, kind);
                true
            }
            JupyterMessageContent::ExecuteResult(result) => {
                if let Some(image) = new_image {
                    output.images.push(image);
                    true
                } else if let Some(text) = media_to_text(&result.data) {
                    output.push_text(&text, OutputKind::Result);
                    true
                } else {
                    false
                }
            }
            JupyterMessageContent::DisplayData(data) => {
                if let Some(image) = new_image {
                    output.images.push(image);
                    true
                } else if let Some(text) = media_to_text(&data.data) {
                    output.push_text(&text, OutputKind::Result);
                    true
                } else {
                    false
                }
            }
            JupyterMessageContent::ErrorOutput(err) => {
                output.state = ExecutionState::Error;
                if err.traceback.is_empty() {
                    output.push_text(&format!("{}: {}", err.ename, err.evalue), OutputKind::Error);
                } else {
                    for line in &err.traceback {
                        output.push_text(&strip_ansi(line), OutputKind::Error);
                    }
                }
                true
            }
            JupyterMessageContent::Status(status) => {
                if matches!(status.execution_state, KernelStatus::Idle) {
                    if output.state == ExecutionState::Running {
                        output.state = ExecutionState::Done;
                    }
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    /// Find the output block targeted by `execution_id` across all documents,
    /// returning whether it matched the introspection follow-up rather than the
    /// primary execution.
    fn find_target(&mut self, execution_id: &str) -> Option<(&mut JupyterOutput, bool)> {
        self.documents_mut()
            .flat_map(|doc| doc.jupyter_outputs.iter_mut())
            .find_map(|output| {
                if output.execution_id == execution_id {
                    Some((output, false))
                } else if output.inspect_execution_id.as_deref() == Some(execution_id) {
                    Some((output, true))
                } else {
                    None
                }
            })
    }

    /// Find the output block for an execution across all documents (for stdin).
    fn find_output_mut(&mut self, execution_id: &str) -> Option<&mut JupyterOutput> {
        self.find_target(execution_id).map(|(output, _)| output)
    }
}
