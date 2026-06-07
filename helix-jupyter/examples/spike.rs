//! Phase 0 spike: spawn a kernel, run code, print iopub output.
//!
//! Run with a registered kernelspec name, e.g.:
//!     cargo run -p helix-jupyter --example spike -- helix-test

use std::time::Duration;

use helix_jupyter::{JupyterMessageContent, Payload, Registry};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let kernel_name = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "python3".to_string());
    eprintln!("starting kernel {kernel_name}...");

    let mut registry = Registry::new();
    let id = registry.start_client_blocking(&kernel_name)?;
    eprintln!("kernel started: {id}");

    let exec1 = registry
        .get_client(id)
        .unwrap()
        .execute("x = 40 + 2".to_string(), false)?;
    let exec2 = registry
        .get_client(id)
        .unwrap()
        .execute("print('value is', x)\nx".to_string(), false)?;
    eprintln!("submitted executions: {exec1}, {exec2}");

    use futures_util::StreamExt;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let next = tokio::time::timeout_at(deadline, registry.incoming.next()).await;
        let Ok(Some((_id, payload))) = next else {
            eprintln!("done (timeout or stream end)");
            break;
        };
        match payload {
            Payload::IoPub(msg) => {
                let parent = msg
                    .parent_header
                    .as_ref()
                    .map(|h| h.msg_id.as_str())
                    .unwrap_or("?");
                match msg.content {
                    JupyterMessageContent::StreamContent(s) => {
                        println!(
                            "[iopub/stream parent={parent}] {:?}: {}",
                            s.name,
                            s.text.trim_end()
                        );
                    }
                    JupyterMessageContent::ExecuteResult(r) => {
                        let text = helix_jupyter::media_to_text(&r.data).unwrap_or_default();
                        println!("[iopub/result parent={parent}] => {text}");
                    }
                    JupyterMessageContent::ErrorOutput(e) => {
                        println!("[iopub/error parent={parent}] {}: {}", e.ename, e.evalue);
                    }
                    JupyterMessageContent::Status(s) => {
                        println!("[iopub/status parent={parent}] {:?}", s.execution_state);
                    }
                    other => {
                        println!("[iopub parent={parent}] {}", other.message_type());
                    }
                }
            }
            Payload::Shell(msg) => {
                println!("[shell] {}", msg.content.message_type());
            }
            Payload::Control(msg) => println!("[control] {}", msg.content.message_type()),
            Payload::Stdin(msg) => println!("[stdin] {}", msg.content.message_type()),
        }
    }

    Ok(())
}
