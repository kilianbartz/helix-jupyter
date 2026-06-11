use super::{Context, Editor};
use crate::ui;

use std::time::Duration;

use anyhow::{anyhow, bail};
use helix_jupyter::registry::KernelId;
use helix_jupyter::{KernelStart, StartOutcome};
use helix_view::jupyter::{JupyterOutput, PendingEval};
use helix_view::DocumentId;

/// Resolve the kernelspec name to start when `:jupyter-start` is given no
/// argument (and for auto-start). Prefers the kernel of an activated Python
/// virtualenv that ships Jupyter, then falls back to the configured
/// `editor.jupyter.default-kernel`.
pub fn default_kernel_name(editor: &Editor) -> Option<String> {
    helix_jupyter::active_venv_kernel().or_else(|| editor.config().jupyter.default_kernel.clone())
}

/// Auto-start a kernel for the document (from the active venv or
/// `editor.jupyter.default-kernel`) when configured, returning the new `Starting`
/// kernel's id. Errors if the feature is disabled, auto-start is off, or no
/// kernel can be resolved.
fn begin_auto_start(editor: &mut Editor, doc_id: DocumentId) -> anyhow::Result<KernelId> {
    let config = editor.config().jupyter.clone();
    if !config.enable {
        bail!("Jupyter integration is disabled (editor.jupyter.enable = false)");
    }
    if !config.auto_start {
        bail!("No running kernel. Start one with :jupyter-start <kernel>");
    }
    let name = default_kernel_name(editor).ok_or_else(|| {
        anyhow!(
            "No kernel selected. Activate a Jupyter venv, set editor.jupyter.default-kernel, or run :jupyter-start <kernel>"
        )
    })?;
    Ok(begin_kernel_start(editor, doc_id, &name))
}

/// Begin starting a kernel for the document without blocking the editor. Returns
/// the (`Starting`) kernel id immediately; the kernel becomes usable once the
/// background start completes and [`on_kernel_started`] promotes it.
pub fn begin_kernel_start(editor: &mut Editor, doc_id: DocumentId, kernel_name: &str) -> KernelId {
    let (id, mut done_rx) = editor.jupyter.start_client(kernel_name);
    if let Some(doc) = editor.documents.get_mut(&doc_id) {
        doc.jupyter_kernel = Some(id);
    }
    editor.set_status(format!("Starting Jupyter kernel '{kernel_name}'…"));

    // The kernel emits no events while booting, so nothing would re-render the
    // status-line spinner. Nudge a redraw on an interval until the start finishes.
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut done_rx => break,
                _ = tokio::time::sleep(Duration::from_millis(80)) => helix_event::request_redraw(),
            }
        }
    });

    id
}

/// Start a kernel for the document (used by `:jupyter-start` and the kernel
/// picker). Non-blocking; see [`begin_kernel_start`].
pub fn jupyter_start_impl(editor: &mut Editor, doc_id: DocumentId, kernel_name: &str) -> KernelId {
    begin_kernel_start(editor, doc_id, kernel_name)
}

/// Apply a completed background kernel start: report status, and replay (or drop)
/// any evaluations queued while the kernel was booting.
pub fn on_kernel_started(editor: &mut Editor, start: KernelStart) {
    match editor.jupyter.finish_start(start) {
        StartOutcome::Started { id, name } => {
            editor.set_status(format!("Started Jupyter kernel '{name}'"));
            for ev in take_pending_evals(editor, id) {
                run_eval(editor, ev.doc_id, id, ev.code, ev.anchor, ev.last_line);
            }
        }
        StartOutcome::Failed { id, name, error } => {
            editor.set_error(format!("Failed to start kernel '{name}': {error}"));
            // Detach the failed kernel from any document and drop its queued evals.
            for doc in editor.documents.values_mut() {
                if doc.jupyter_kernel == Some(id) {
                    doc.jupyter_kernel = None;
                }
            }
            editor.jupyter_pending_evals.retain(|ev| ev.kernel != id);
        }
        StartOutcome::Cancelled { id } => {
            editor.jupyter_pending_evals.retain(|ev| ev.kernel != id);
        }
    }
}

/// Remove and return the queued evaluations belonging to `kernel`, preserving order.
fn take_pending_evals(editor: &mut Editor, kernel: KernelId) -> Vec<PendingEval> {
    let mut mine = Vec::new();
    let mut keep = Vec::new();
    for ev in editor.jupyter_pending_evals.drain(..) {
        if ev.kernel == kernel {
            mine.push(ev);
        } else {
            keep.push(ev);
        }
    }
    editor.jupyter_pending_evals = keep;
    mine
}

/// Clear a document's Jupyter outputs, queuing all their inline images for
/// deletion from the terminal on the next render.
fn clear_outputs_and_queue_images(editor: &mut Editor, doc_id: DocumentId) {
    let ids: Vec<u32> = editor
        .document_mut(doc_id)
        .map(|doc| {
            let ids = doc
                .jupyter_outputs
                .iter()
                .flat_map(|o| o.images.iter().map(|img| img.id))
                .collect();
            doc.jupyter_outputs.clear();
            ids
        })
        .unwrap_or_default();
    editor.jupyter_pending_image_deletions.extend(ids);
}

/// Evaluate the current selection (or current line) in the document's kernel.
pub fn jupyter_eval(cx: &mut Context) {
    jupyter_eval_impl(cx.editor);
}

pub fn jupyter_eval_impl(editor: &mut Editor) {
    let (view, doc) = current!(editor);
    let doc_id = doc.id();
    let slice = doc.text().slice(..);
    let range = doc.selection(view.id).primary();

    // Evaluate the *whole lines* spanned by the selection. A bare cursor in
    // Helix is a 1-width selection, so evaluating the raw fragment would run a
    // single character; line-wise evaluation matches "select some lines and run".
    let start_line = slice.char_to_line(range.from());
    let end_line = if range.to() > range.from() {
        slice.char_to_line(range.to().saturating_sub(1))
    } else {
        start_line
    };
    eval_line_range(editor, doc_id, start_line, end_line);
}

/// Evaluate the inclusive document line range `start_line..=end_line` in the
/// document's kernel, auto-starting or queueing if it isn't ready yet. The
/// output block is anchored below `end_line`.
fn eval_line_range(editor: &mut Editor, doc_id: DocumentId, start_line: usize, end_line: usize) {
    let Some(doc) = editor.documents.get(&doc_id) else {
        return;
    };
    let slice = doc.text().slice(..);
    let from_char = slice.line_to_char(start_line);
    let to_char = slice.line_to_char((end_line + 1).min(slice.len_lines()));
    let code = slice.slice(from_char..to_char).to_string();
    if code.trim().is_empty() {
        editor.set_error("Nothing to evaluate");
        return;
    }

    // Anchor the output below the last evaluated line.
    let last_line = end_line;
    let anchor = from_char.max(slice.line_to_char(end_line));

    // If a kernel is already connected, evaluate immediately.
    if let Some(kernel) = editor
        .documents
        .get(&doc_id)
        .and_then(|doc| doc.jupyter_kernel)
    {
        if editor.jupyter.get_client(kernel).is_some() {
            run_eval(editor, doc_id, kernel, code, anchor, last_line);
            return;
        }
        // The kernel is still booting: defer this evaluation until it is ready.
        if editor.jupyter.is_starting(kernel) {
            editor.jupyter_pending_evals.push(PendingEval {
                kernel,
                doc_id,
                code,
                anchor,
                last_line,
            });
            editor.set_status("Kernel is starting; evaluation queued");
            return;
        }
    }

    // No running or starting kernel: auto-start one (if configured) and queue the
    // evaluation to run once it connects.
    let kernel = match begin_auto_start(editor, doc_id) {
        Ok(kernel) => kernel,
        Err(err) => {
            editor.set_error(err.to_string());
            return;
        }
    };
    editor.jupyter_pending_evals.push(PendingEval {
        kernel,
        doc_id,
        code,
        anchor,
        last_line,
    });
}

/// Evaluate the percent-format cell (`# %%` block) under the cursor.
pub fn jupyter_eval_cell(cx: &mut Context) {
    jupyter_eval_cell_impl(cx.editor);
}

pub fn jupyter_eval_cell_impl(editor: &mut Editor) {
    use helix_view::notebook::{cell_at_line, CellKind};

    let (view, doc) = current!(editor);
    let doc_id = doc.id();
    let text = doc.text().slice(..);
    let cursor_line = doc.selection(view.id).primary().cursor_line(text);

    if doc.cell_spans().is_empty() {
        editor.set_error("No cell at cursor (no '# %%' markers in buffer)");
        return;
    }
    let Some(cell) = cell_at_line(doc.cell_spans(), cursor_line).copied() else {
        editor.set_error("No cell at cursor");
        return;
    };
    match cell.kind {
        CellKind::Markdown | CellKind::Raw => {
            editor.set_status("Markdown/raw cell — not evaluated");
            return;
        }
        CellKind::Code => {}
    }
    if cell.start_line >= cell.end_line {
        editor.set_error("Nothing to evaluate (empty cell)");
        return;
    }
    // The delimiter line is excluded; the output anchors below the last body
    // line, replacing any previous output of this cell.
    eval_line_range(editor, doc_id, cell.start_line, cell.end_line - 1);
}

/// Move the cursor to the first body line of the next/previous cell.
fn goto_cell_impl(cx: &mut Context, forward: bool) {
    use helix_core::Selection;

    let (view, doc) = current!(cx.editor);
    let text = doc.text().slice(..);
    let cursor_line = doc.selection(view.id).primary().cursor_line(text);

    let cells = doc.cell_spans();
    let no_cells = cells.is_empty();
    let current = cells
        .iter()
        .position(|cell| cell.contains_line(cursor_line));
    let target = if forward {
        match current {
            Some(i) => cells.get(i + 1),
            None => cells.iter().find(|cell| cell.marker_line > cursor_line),
        }
    } else {
        match current {
            Some(i) => i.checked_sub(1).and_then(|i| cells.get(i)),
            None => cells.iter().rev().find(|cell| cell.end_line <= cursor_line),
        }
    };
    let Some(cell) = target.copied() else {
        cx.editor.set_status(if no_cells {
            "No '# %%' cells in buffer"
        } else if forward {
            "Already at the last cell"
        } else {
            "Already at the first cell"
        });
        return;
    };
    // Land on the first body line; for an empty cell, on its delimiter.
    let line = if cell.start_line < cell.end_line {
        cell.start_line
    } else {
        cell.marker_line
    };
    let pos = text.line_to_char(line.min(text.len_lines().saturating_sub(1)));
    doc.set_selection(view.id, Selection::point(pos));
}

pub fn goto_next_cell(cx: &mut Context) {
    goto_cell_impl(cx, true);
}

pub fn goto_prev_cell(cx: &mut Context) {
    goto_cell_impl(cx, false);
}

/// Execute `code` in the document's (ready) `kernel`, firing the optional
/// variable-introspection follow-up and creating the output block anchored below
/// the last evaluated line (replacing any previous output on that line).
fn run_eval(
    editor: &mut Editor,
    doc_id: DocumentId,
    kernel: KernelId,
    code: String,
    anchor: usize,
    last_line: usize,
) {
    let execution_id = match editor.jupyter.get_client(kernel) {
        Some(client) => match client.execute(code.clone(), false) {
            Ok(id) => id,
            Err(err) => {
                editor.set_error(format!("Evaluation failed: {err}"));
                return;
            }
        },
        None => {
            editor.set_error("Kernel is not running");
            return;
        }
    };

    // Optionally fire a silent follow-up that introspects the variables touched
    // by the selection, so they can be shown in the inspector panel.
    let inspect_execution_id = if editor.config().jupyter.inspect_variables {
        let names = extract_identifiers(&code);
        if names.is_empty() {
            None
        } else {
            let probe = introspection_code(&names);
            editor
                .jupyter
                .get_client(kernel)
                .and_then(|client| client.execute_quiet(probe).ok())
        }
    } else {
        None
    };

    let removed_image_ids: Vec<u32> = if let Some(doc) = editor.document_mut(doc_id) {
        // Replace any previous output anchored to the same line.
        let text = doc.text().clone();
        let len = text.len_chars();
        let on_line = |o: &JupyterOutput| text.char_to_line(o.anchor.min(len)) == last_line;
        let removed = doc
            .jupyter_outputs
            .iter()
            .filter(|o| on_line(o))
            .flat_map(|o| o.images.iter().map(|img| img.id))
            .collect();
        doc.jupyter_outputs.retain(|o| !on_line(o));
        let mut output = JupyterOutput::new(anchor, execution_id, kernel);
        output.inspect_execution_id = inspect_execution_id;
        doc.jupyter_outputs.push(output);
        removed
    } else {
        Vec::new()
    };
    // Free the replaced blocks' images from the terminal on the next render.
    editor
        .jupyter_pending_image_deletions
        .extend(removed_image_ids);
    helix_event::request_redraw();
}

/// Python keywords and a few common builtins to exclude from variable probing.
const PY_NON_VARIABLES: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield", "match", "case", "print", "self", "cls",
];

/// Extract candidate variable names (identifiers) from a code selection,
/// preserving order and removing duplicates and Python keywords. Over-approximates
/// to all referenced identifiers; the kernel-side probe filters to names that
/// actually exist as non-callable globals.
fn extract_identifiers(code: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut current = String::new();
    let bytes = code.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_alphanumeric() || c == '_' {
            current.push(c);
        } else {
            push_identifier(&mut names, &mut current);
            // Skip attribute access chains (`a.b`) and string/number contexts are
            // naturally handled because `.` is a separator and digits-only tokens
            // are filtered below.
            if c == '.' {
                // Drop the identifier that follows a dot (attribute, not a variable)
                // by consuming it here.
                i += 1;
                while i < bytes.len()
                    && ((bytes[i] as char).is_alphanumeric() || bytes[i] as char == '_')
                {
                    i += 1;
                }
                continue;
            }
        }
        i += 1;
    }
    push_identifier(&mut names, &mut current);
    names
}

fn push_identifier(names: &mut Vec<String>, current: &mut String) {
    if current.is_empty() {
        return;
    }
    let ident = std::mem::take(current);
    let first_is_digit = ident.chars().next().is_some_and(|c| c.is_ascii_digit());
    if !first_is_digit && !PY_NON_VARIABLES.contains(&ident.as_str()) && !names.contains(&ident) {
        names.push(ident);
    }
}

/// Build a one-liner that prints a JSON object of `{name: repr(value)}` for each
/// name that exists as a non-callable, non-module global. Leaves no bindings.
fn introspection_code(names: &[String]) -> String {
    let list = names
        .iter()
        .map(|n| format!("'{n}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "print(__import__('json').dumps({{n: repr(globals()[n]) for n in [{list}] \
         if n in globals() and not callable(globals()[n]) \
         and type(globals()[n]).__name__ != 'module'}}), end='')"
    )
}

/// Build the variable-inspector popup for the output block at (or nearest above)
/// the cursor, falling back to the most recent block that has variables. Returns
/// `None` when there are no variables to show.
pub fn variables_popup(editor: &Editor) -> Option<ui::Popup<ui::Text>> {
    use helix_view::theme::Style;
    use tui::text::{Span, Spans};

    let (view, doc) = current_ref!(editor);
    let text = doc.text().slice(..);
    let cursor_line = doc.selection(view.id).primary().cursor_line(text);
    let len = text.len_chars();

    let on_line = doc
        .jupyter_outputs
        .iter()
        .find(|o| !o.variables.is_empty() && text.char_to_line(o.anchor.min(len)) == cursor_line);
    let output = on_line.or_else(|| {
        doc.jupyter_outputs
            .iter()
            .rev()
            .find(|o| !o.variables.is_empty())
    })?;

    let name_style: Style = editor.theme.get("ui.text.focus");
    let value_style: Style = editor.theme.get("ui.text");

    let lines: Vec<Spans> = output
        .variables
        .iter()
        .map(|(name, value)| {
            Spans::from(vec![
                Span::styled(name.clone(), name_style),
                Span::raw(" = "),
                Span::styled(value.clone(), value_style),
            ])
        })
        .collect();

    let contents = ui::Text::from(tui::text::Text::from(lines));
    Some(ui::Popup::new("jupyter-variables", contents))
}

/// Build a picker over installed kernelspecs that starts the chosen kernel for
/// the current document. Returns `None` if no kernelspecs are installed.
pub fn kernel_picker(editor: &Editor) -> Option<Box<dyn crate::compositor::Component>> {
    use helix_jupyter::KernelSpec;

    let doc_id = doc!(editor).id();
    let kernels = helix_jupyter::available_kernels();
    if kernels.is_empty() {
        return None;
    }

    let columns = [
        ui::PickerColumn::new("name", |k: &KernelSpec, _| k.name.as_str().into()),
        ui::PickerColumn::new("display name", |k: &KernelSpec, _| {
            k.display_name.as_str().into()
        }),
        ui::PickerColumn::new("language", |k: &KernelSpec, _| k.language.as_str().into()),
    ];

    let picker = ui::Picker::new(columns, 0, kernels, (), move |cx, kernel, _action| {
        jupyter_start_impl(cx.editor, doc_id, &kernel.name);
    });

    Some(Box::new(ui::overlay::overlaid(picker)))
}

/// Pick a kernelspec to start for the current document.
pub fn jupyter_kernel_select(cx: &mut Context) {
    match kernel_picker(cx.editor) {
        Some(layer) => cx.push_layer(layer),
        None => cx.editor.set_error("No Jupyter kernelspecs found"),
    }
}

/// Show the variable inspector for the current document.
pub fn jupyter_variables(cx: &mut Context) {
    match variables_popup(cx.editor) {
        Some(popup) => cx.replace_or_push_layer("jupyter-variables", popup),
        None => cx
            .editor
            .set_status("No Jupyter variables to show (evaluate a selection first)"),
    }
}

/// Stop the document's kernel.
pub fn jupyter_stop(cx: &mut Context) {
    jupyter_stop_impl(cx.editor);
}

pub fn jupyter_stop_impl(editor: &mut Editor) {
    let doc = doc!(editor);
    let doc_id = doc.id();
    let Some(kernel) = doc.jupyter_kernel else {
        editor.set_error("No kernel running for this document");
        return;
    };
    if let Some(client) = editor.jupyter.get_client(kernel) {
        let _ = client.shutdown(false);
    }
    editor.jupyter.remove_client(kernel);
    editor
        .jupyter_pending_evals
        .retain(|ev| ev.kernel != kernel);
    if let Some(doc) = editor.document_mut(doc_id) {
        doc.jupyter_kernel = None;
    }
    clear_outputs_and_queue_images(editor, doc_id);
    editor.set_status("Stopped Jupyter kernel");
}

/// Restart the document's kernel (clears persisted state and output).
pub fn jupyter_restart(cx: &mut Context) {
    jupyter_restart_impl(cx.editor);
}

pub fn jupyter_restart_impl(editor: &mut Editor) {
    let doc = doc!(editor);
    let doc_id = doc.id();
    let Some(kernel) = doc.jupyter_kernel else {
        editor.set_error("No kernel running for this document");
        return;
    };
    let name = editor
        .jupyter
        .get_client(kernel)
        .map(|client| client.name().to_string());
    editor.jupyter.remove_client(kernel);
    editor
        .jupyter_pending_evals
        .retain(|ev| ev.kernel != kernel);
    if let Some(doc) = editor.document_mut(doc_id) {
        doc.jupyter_kernel = None;
    }
    clear_outputs_and_queue_images(editor, doc_id);
    let Some(name) = name else {
        editor.set_error("Could not determine kernel to restart");
        return;
    };
    begin_kernel_start(editor, doc_id, &name);
}
