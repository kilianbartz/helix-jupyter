use std::time::{Duration, Instant};

use helix_view::doc;
use helix_view::editor::EditorEvent;
use helix_view::jupyter::{ExecutionState, JupyterOutput};

use super::helpers::AppBuilder;

/// End-to-end check of the Jupyter integration through the real `Editor` event
/// path: start a kernel, run two executions (proving state persists), pump the
/// editor event loop, and assert the output is routed into the document's
/// `jupyter_outputs` by `handle_jupyter_message`.
///
/// Requires a kernelspec named `helix-test`; the test skips itself if none is
/// installed so it doesn't fail on machines without Jupyter.
#[tokio::test(flavor = "multi_thread")]
async fn jupyter_eval_routes_output_to_document() -> anyhow::Result<()> {
    let mut app = AppBuilder::new()
        .with_input_text("#[x|]# = 40 + 2")
        .build()?;
    let editor = &mut app.editor;
    let doc_id = doc!(editor).id();

    let kernel = match editor.jupyter.start_client("helix-test") {
        Ok(kernel) => kernel,
        Err(err) => {
            eprintln!("skipping jupyter test, `helix-test` kernel unavailable: {err}");
            return Ok(());
        }
    };
    editor.document_mut(doc_id).unwrap().jupyter_kernel = Some(kernel);

    // First execution defines `x`; the second reads it back, proving the kernel
    // keeps state between evaluations.
    let _ = editor
        .jupyter
        .get_client(kernel)
        .unwrap()
        .execute("x = 40 + 2".to_string(), false)?;
    let exec = editor
        .jupyter
        .get_client(kernel)
        .unwrap()
        .execute("print('value is', x)\nx".to_string(), false)?;
    editor
        .document_mut(doc_id)
        .unwrap()
        .jupyter_outputs
        .push(JupyterOutput::new(0, exec.clone(), kernel));

    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let finished = editor
            .documents
            .get(&doc_id)
            .and_then(|doc| doc.jupyter_outputs.iter().find(|o| o.execution_id == exec))
            .is_some_and(|o| o.state != ExecutionState::Running);
        if finished || Instant::now() >= deadline {
            break;
        }
        if let Ok(event) =
            tokio::time::timeout(Duration::from_millis(500), editor.wait_event()).await
        {
            if let EditorEvent::JupyterEvent((id, payload)) = event {
                editor.handle_jupyter_message(id, payload).await;
            }
        }
    }

    let output = editor
        .documents
        .get(&doc_id)
        .unwrap()
        .jupyter_outputs
        .iter()
        .find(|o| o.execution_id == exec)
        .expect("output block should exist");

    let text: Vec<&str> = output.lines.iter().map(|l| l.text.as_str()).collect();
    let joined = text.join("\n");
    assert_eq!(
        output.state,
        ExecutionState::Done,
        "execution should have completed; output: {joined:?}"
    );
    assert!(
        joined.contains("value is"),
        "expected stdout 'value is' in output, got: {joined:?}"
    );
    assert!(
        joined.contains("42"),
        "expected result '42' in output, got: {joined:?}"
    );

    editor.jupyter.remove_client(kernel);
    Ok(())
}

/// Verify the variable-introspection follow-up: a silent probe execution's JSON
/// result is routed to `JupyterOutput::variables` via `inspect_execution_id`.
#[tokio::test(flavor = "multi_thread")]
async fn jupyter_variable_introspection() -> anyhow::Result<()> {
    use helix_jupyter::registry::Registry;

    let mut registry = Registry::new();
    let kernel = match registry.start_client("helix-test") {
        Ok(kernel) => kernel,
        Err(err) => {
            eprintln!("skipping jupyter test, `helix-test` kernel unavailable: {err}");
            return Ok(());
        }
    };

    let client = registry.get_client(kernel).unwrap();
    // Define two variables, then probe them with the same code the editor uses.
    let _ = client.execute("a = 7\nb = 'hello'".to_string(), false)?;
    let probe = "print(__import__('json').dumps({n: repr(globals()[n]) for n in ['a', 'b'] \
         if n in globals() and not callable(globals()[n]) \
         and type(globals()[n]).__name__ != 'module'}), end='')"
        .to_string();
    let probe_id = client.execute_quiet(probe)?;

    let mut output = JupyterOutput::new(0, "unused".to_string(), kernel);
    output.inspect_execution_id = Some(probe_id.clone());

    use futures_util::StreamExt;
    use helix_jupyter::{JupyterMessageContent, Payload};
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if output.state != ExecutionState::Running && !output.inspect_buffer.is_empty() {
            // got buffer; wait for idle handled below
        }
        if Instant::now() >= deadline {
            break;
        }
        let Ok(Some((_id, payload))) =
            tokio::time::timeout(Duration::from_millis(500), registry.incoming.next()).await
        else {
            continue;
        };
        if let Payload::IoPub(msg) = payload {
            let parent = msg.parent_header.as_ref().map(|h| h.msg_id.clone());
            if parent.as_deref() != Some(probe_id.as_str()) {
                continue;
            }
            match msg.content {
                JupyterMessageContent::StreamContent(stream) => {
                    output.inspect_buffer.push_str(&stream.text)
                }
                JupyterMessageContent::Status(status)
                    if matches!(status.execution_state, helix_jupyter::ExecutionState::Idle) =>
                {
                    output.parse_inspect_buffer();
                    break;
                }
                _ => {}
            }
        }
    }

    registry.remove_client(kernel);

    assert!(
        output.variables.iter().any(|(n, v)| n == "a" && v == "7"),
        "expected a=7 in variables, got: {:?}",
        output.variables
    );
    assert!(
        output.variables.iter().any(|(n, _)| n == "b"),
        "expected b in variables, got: {:?}",
        output.variables
    );
    Ok(())
}
