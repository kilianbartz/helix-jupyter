//! Jupyter notebook (`.ipynb`) support: percent-format cell scanning and the
//! nbformat JSON ↔ text conversion used to edit notebooks as plain buffers.
//!
//! A buffer is divided into cells by jupytext-style delimiter lines (`# %%`,
//! `# %% [markdown]`, `# %% [raw]`). The scan is purely textual, so cell
//! commands and rendering work in any buffer containing markers (e.g. a plain
//! `.py` in percent format), not just converted notebooks.

use helix_core::RopeSlice;
use serde_json::Value;

/// Retained nbformat state for a document opened from an `.ipynb` file. The
/// JSON is the parsed notebook as last loaded/saved; on save, the buffer's
/// cell sources are patched into a clone of it so everything the editor does
/// not model (notebook/cell metadata, ids, outputs, attachments) round-trips.
#[derive(Debug, Clone)]
pub struct NotebookFile {
    pub json: serde_json::Value,
}

/// The nbformat cell type a delimiter announces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellKind {
    Code,
    Markdown,
    Raw,
}

/// One cell of a percent-format buffer, in document line indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellSpan {
    pub kind: CellKind,
    /// Line holding the `# %%` delimiter. For an implicit leading cell (text
    /// before the first marker) this equals `start_line`.
    pub marker_line: usize,
    /// First body line (`marker_line + 1`, or 0 for an implicit leading cell).
    pub start_line: usize,
    /// One past the last body line (the next marker line, or `len_lines`).
    pub end_line: usize,
}

impl CellSpan {
    /// Whether `line` belongs to this cell (delimiter included).
    pub fn contains_line(&self, line: usize) -> bool {
        self.marker_line.min(self.start_line) <= line && line < self.end_line
    }
}

/// Parse a line as a cell delimiter, returning the announced cell kind.
/// Jupytext-compatible: the line must start with `# %%` or `#%%`; a
/// `[markdown]`/`[md]` tag makes it a markdown cell, `[raw]` a raw cell.
/// Anything else after the marker (cell titles) is allowed and ignored.
pub fn parse_marker(line: RopeSlice) -> Option<CellKind> {
    // Markers are short; flatten the (almost always contiguous) line prefix.
    let prefix: String = line.chars().take(64).collect();
    let rest = prefix
        .strip_prefix("# %%")
        .or_else(|| prefix.strip_prefix("#%%"))?;
    // `# %%%`/`# %%-` style lines are not markers; jupytext requires the
    // marker to be followed by whitespace or end of line.
    if rest.chars().next().is_some_and(|c| !c.is_whitespace()) {
        return None;
    }
    let rest = rest.trim();
    let kind = if rest.starts_with("[markdown]") || rest.starts_with("[md]") {
        CellKind::Markdown
    } else if rest.starts_with("[raw]") {
        CellKind::Raw
    } else {
        CellKind::Code
    };
    Some(kind)
}

/// Split a buffer into cells at its `# %%` delimiter lines.
///
/// Returns an empty vec when the buffer contains no markers (it is not a
/// percent-format buffer). Non-blank text before the first marker becomes an
/// implicit leading code cell whose `marker_line == start_line`.
pub fn scan_cells(text: RopeSlice) -> Vec<CellSpan> {
    let mut markers: Vec<(usize, CellKind)> = Vec::new();
    for (line_idx, line) in text.lines().enumerate() {
        if let Some(kind) = parse_marker(line) {
            markers.push((line_idx, kind));
        }
    }
    if markers.is_empty() {
        return Vec::new();
    }

    let mut cells = Vec::with_capacity(markers.len() + 1);
    let first_marker = markers[0].0;
    // Text before the first marker forms an implicit leading code cell, but
    // only if any of it is non-blank.
    if first_marker > 0 {
        let leading_non_blank =
            (0..first_marker).any(|l| text.line(l).chars().any(|c| !c.is_whitespace()));
        if leading_non_blank {
            cells.push(CellSpan {
                kind: CellKind::Code,
                marker_line: 0,
                start_line: 0,
                end_line: first_marker,
            });
        }
    }
    for (i, &(marker_line, kind)) in markers.iter().enumerate() {
        let end_line = markers
            .get(i + 1)
            .map(|&(next, _)| next)
            .unwrap_or_else(|| text.len_lines());
        cells.push(CellSpan {
            kind,
            marker_line,
            start_line: marker_line + 1,
            end_line,
        });
    }
    cells
}

// ---------------------------------------------------------------------------
// nbformat JSON ↔ percent-format text conversion
// ---------------------------------------------------------------------------

/// Parse the contents of an `.ipynb` file, accepting only values with the
/// nbformat shape (an object with a `cells` array and an `nbformat` version).
/// Anything else (including valid JSON that isn't a notebook) yields `None`
/// and the file is edited as plain JSON.
pub fn parse_notebook(src: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(src).ok()?;
    let is_notebook = value.is_object()
        && value.get("cells").is_some_and(Value::is_array)
        && value.get("nbformat").is_some_and(Value::is_number);
    is_notebook.then_some(value)
}

/// Render a parsed notebook as percent-format text: every cell becomes a
/// `# %%` delimiter line followed by its source and exactly one appended
/// newline (so a conventional source without trailing newline reads back with
/// a blank separator line before the next delimiter). Markdown/raw sources
/// are `# `-commented per line.
pub fn notebook_to_percent(nb: &Value) -> String {
    static EMPTY: Vec<Value> = Vec::new();
    let cells = nb.get("cells").and_then(Value::as_array).unwrap_or(&EMPTY);
    let mut out = String::new();
    for cell in cells {
        let kind = json_cell_kind(cell);
        out.push_str(match kind {
            CellKind::Code => "# %%",
            CellKind::Markdown => "# %% [markdown]",
            CellKind::Raw => "# %% [raw]",
        });
        out.push('\n');
        let source = json_text(cell.get("source").unwrap_or(&Value::Null));
        match kind {
            CellKind::Code => out.push_str(&source),
            CellKind::Markdown | CellKind::Raw => out.push_str(&encode_comment(&source)),
        }
        out.push('\n');
    }
    out
}

/// Patch the buffer's cell sources back into a clone of the retained notebook
/// JSON. Buffer cells are matched against retained cells (see [`match_cells`]);
/// matched cells keep their id, metadata, execution count, outputs and
/// attachments, unmatched buffer cells become fresh cells, and retained cells
/// matched by nothing are dropped.
///
/// `doc_outputs` are the document's current inline output blocks: a cell that
/// was re-executed in the editor (it has a kernel-produced block anchored
/// inside it) gets that block written back as its nbformat `outputs`,
/// replacing the stored ones; all other cells keep their stored outputs.
pub fn serialize_notebook(
    text: RopeSlice,
    retained: &Value,
    doc_outputs: &[crate::jupyter::JupyterOutput],
) -> anyhow::Result<Value> {
    anyhow::ensure!(retained.is_object(), "retained notebook is not an object");
    static EMPTY: Vec<Value> = Vec::new();
    let retained_cells = retained
        .get("cells")
        .and_then(Value::as_array)
        .unwrap_or(&EMPTY);
    let buffer = buffer_cells(text);
    let matches = match_cells(&buffer, retained_cells);

    // Kernel-produced output blocks by the cell (buffer index) they sit in.
    let spans = scan_cells(text);
    let executed_outputs = |cell_index: usize| -> Vec<&crate::jupyter::JupyterOutput> {
        let Some(span) = spans.get(cell_index) else {
            return Vec::new();
        };
        doc_outputs
            .iter()
            .filter(|output| {
                !output.execution_id.starts_with(STORED_OUTPUT_PREFIX) && {
                    let line = text.char_to_line(output.anchor.min(text.len_chars()));
                    span.contains_line(line)
                }
            })
            .collect()
    };

    // nbformat ≥ 4.5 requires per-cell ids.
    let version = (
        retained
            .get("nbformat")
            .and_then(Value::as_i64)
            .unwrap_or(4),
        retained
            .get("nbformat_minor")
            .and_then(Value::as_i64)
            .unwrap_or(0),
    );
    let uses_ids = version >= (4, 5) || retained_cells.iter().any(|cell| cell.get("id").is_some());

    let mut cells = Vec::with_capacity(buffer.len());
    for (index, ((kind, source), matched)) in buffer.iter().zip(&matches).enumerate() {
        let mut cell = match matched {
            Some(j) => retained_cells[*j].clone(),
            None => fresh_cell(*kind, uses_ids, index, source),
        };
        // Leave an unchanged source untouched so its original representation
        // (plain string vs list of lines) round-trips byte-for-byte.
        if cell_source(&cell) != *source {
            cell["source"] = source_to_json(source);
        }
        if *kind == CellKind::Code {
            let executed = executed_outputs(index);
            if !executed.is_empty() {
                cell["outputs"] = Value::Array(
                    executed
                        .iter()
                        .flat_map(|output| outputs_to_json(output))
                        .collect(),
                );
                // We don't track kernel execution counts.
                cell["execution_count"] = Value::Null;
            }
        }
        cells.push(cell);
    }
    let mut nb = retained.clone();
    nb["cells"] = Value::Array(cells);
    Ok(nb)
}

/// A minimal nbformat 4.5 notebook built from a buffer, used when a non-notebook
/// buffer is saved to an `.ipynb` path.
pub fn fresh_notebook(text: RopeSlice) -> Value {
    let skeleton = serde_json::json!({
        "cells": [],
        "metadata": {},
        "nbformat": 4,
        "nbformat_minor": 5,
    });
    serialize_notebook(text, &skeleton, &[]).expect("skeleton notebook is an object")
}

/// Serialize a notebook the way `nbformat` does (`json.dump(..., indent=1)`
/// plus a trailing newline), keeping diffs against Jupyter-written files small.
pub fn to_json_string(nb: &Value) -> String {
    use serde::Serialize;

    let mut buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
    nb.serialize(&mut ser)
        .expect("serializing serde_json::Value cannot fail");
    let mut out = String::from_utf8(buf).expect("serde_json emits UTF-8");
    out.push('\n');
    out
}

/// Extract the buffer's cells as `(kind, nbformat source)` pairs: the body
/// lines between delimiters minus exactly one trailing newline (the inverse of
/// [`notebook_to_percent`]), with markdown/raw comment prefixes stripped. A
/// non-blank buffer without any markers degrades to a single code cell so cell
/// deletion can never silently drop buffer content on save.
pub fn buffer_cells(text: RopeSlice) -> Vec<(CellKind, String)> {
    let spans = scan_cells(text);
    if spans.is_empty() {
        let whole = text.to_string();
        if whole.trim().is_empty() {
            return Vec::new();
        }
        let body = whole.strip_suffix('\n').unwrap_or(&whole).to_string();
        return vec![(CellKind::Code, body)];
    }
    spans
        .iter()
        .map(|cell| {
            let from = text.line_to_char(cell.start_line.min(text.len_lines()));
            let to = text.line_to_char(cell.end_line.min(text.len_lines()));
            let body: String = text.slice(from..to).to_string();
            let body = body.strip_suffix('\n').unwrap_or(&body);
            let source = match cell.kind {
                CellKind::Code => body.to_string(),
                CellKind::Markdown | CellKind::Raw => decode_comment(body),
            };
            (cell.kind, source)
        })
        .collect()
}

/// Match buffer cells to retained notebook cells, returning for each buffer
/// cell the index of its retained counterpart (or `None` for new cells).
///
/// Pass 1 matches in order on (kind, exact source), each match restricting
/// later ones to subsequent retained cells. Pass 2 pairs the remaining
/// unmatched buffer cells positionally — in order, same kind — with unmatched
/// retained cells between the surrounding pass-1 anchors, which keeps edited
/// in-place cells attached to their metadata and outputs.
pub fn match_cells(buffer: &[(CellKind, String)], retained: &[Value]) -> Vec<Option<usize>> {
    let mut result = vec![None; buffer.len()];
    let mut used = vec![false; retained.len()];

    let mut next = 0;
    for (i, (kind, source)) in buffer.iter().enumerate() {
        for (j, cell) in retained.iter().enumerate().skip(next) {
            if !used[j] && json_cell_kind(cell) == *kind && cell_source(cell) == *source {
                result[i] = Some(j);
                used[j] = true;
                next = j + 1;
                break;
            }
        }
    }

    // Pass 1b: exact matches anywhere, so reordered cells keep their identity
    // (the monotone pass above misses cells moved before an earlier match).
    for (i, (kind, source)) in buffer.iter().enumerate() {
        if result[i].is_some() {
            continue;
        }
        for (j, cell) in retained.iter().enumerate() {
            if !used[j] && json_cell_kind(cell) == *kind && cell_source(cell) == *source {
                result[i] = Some(j);
                used[j] = true;
                break;
            }
        }
    }

    let mut window_start = 0;
    let mut i = 0;
    while i < buffer.len() {
        if let Some(j) = result[i] {
            window_start = j + 1;
            i += 1;
            continue;
        }
        let run_start = i;
        while i < buffer.len() && result[i].is_none() {
            i += 1;
        }
        let window_end = result.get(i).copied().flatten().unwrap_or(retained.len());
        let mut j = window_start;
        let mut bi = run_start;
        while bi < i {
            let kind = buffer[bi].0;
            while j < window_end && (used[j] || json_cell_kind(&retained[j]) != kind) {
                j += 1;
            }
            if j < window_end {
                result[bi] = Some(j);
                used[j] = true;
                j += 1;
            }
            bi += 1;
        }
    }
    result
}

/// Build a brand-new nbformat cell for a buffer cell with no retained
/// counterpart.
fn fresh_cell(kind: CellKind, uses_ids: bool, index: usize, source: &str) -> Value {
    let mut cell = serde_json::json!({
        "cell_type": match kind {
            CellKind::Code => "code",
            CellKind::Markdown => "markdown",
            CellKind::Raw => "raw",
        },
        "metadata": {},
        "source": [],
    });
    if kind == CellKind::Code {
        cell["execution_count"] = Value::Null;
        cell["outputs"] = serde_json::json!([]);
    }
    if uses_ids {
        cell["id"] = Value::String(generate_cell_id(source, index));
    }
    cell
}

/// 8-hex-char cell id, unique enough within one notebook (nbformat only
/// requires per-notebook uniqueness).
fn generate_cell_id(source: &str, index: usize) -> String {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    index.hash(&mut hasher);
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .hash(&mut hasher);
    format!("{:08x}", hasher.finish() as u32)
}

/// The kind of an nbformat cell (unknown types degrade to raw: their source
/// round-trips, commented).
fn json_cell_kind(cell: &Value) -> CellKind {
    match cell.get("cell_type").and_then(Value::as_str) {
        Some("code") => CellKind::Code,
        Some("markdown") => CellKind::Markdown,
        _ => CellKind::Raw,
    }
}

/// An nbformat cell's source as a single string.
fn cell_source(cell: &Value) -> String {
    json_text(cell.get("source").unwrap_or(&Value::Null))
}

/// nbformat "multiline string": either a string or a list of line strings.
pub fn json_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(Value::as_str).collect::<String>(),
        _ => String::new(),
    }
}

/// A source string as nbformat's list-of-lines representation.
fn source_to_json(source: &str) -> Value {
    Value::Array(
        source
            .split_inclusive('\n')
            .map(|line| Value::String(line.to_string()))
            .collect(),
    )
}

/// Comment a markdown/raw source for embedding in a percent-format buffer:
/// `# ` before each line, a bare `#` for empty lines.
fn encode_comment(source: &str) -> String {
    let mut out = String::new();
    for line in source.split_inclusive('\n') {
        let (content, newline) = match line.strip_suffix('\n') {
            Some(content) => (content, true),
            None => (line, false),
        };
        if content.is_empty() {
            out.push('#');
        } else {
            out.push_str("# ");
            out.push_str(content);
        }
        if newline {
            out.push('\n');
        }
    }
    out
}

/// Inverse of [`encode_comment`], best-effort for hand-edited lines: strips
/// one `# ` (or a bare `#`) per line and keeps unprefixed lines verbatim.
fn decode_comment(body: &str) -> String {
    let mut out = String::new();
    for line in body.split_inclusive('\n') {
        let (content, newline) = match line.strip_suffix('\n') {
            Some(content) => (content, true),
            None => (line, false),
        };
        let stripped = content
            .strip_prefix("# ")
            .or_else(|| content.strip_prefix('#'))
            .unwrap_or(content);
        out.push_str(stripped);
        if newline {
            out.push('\n');
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Stored (pre-computed) cell outputs
// ---------------------------------------------------------------------------

/// Synthetic execution-id prefix marking output blocks loaded from the
/// notebook file rather than produced by a kernel.
pub const STORED_OUTPUT_PREFIX: &str = "notebook:";

/// Populate `doc.jupyter_outputs` with the notebook's stored cell outputs so
/// they render inline like fresh evaluations. Existing stored blocks are
/// replaced; kernel-produced blocks are left alone. Returns the image ids of
/// dropped blocks so the caller can queue their deletion from the terminal.
pub fn load_outputs(
    doc: &mut crate::Document,
    alloc_image_id: &mut dyn FnMut() -> u32,
) -> Vec<u32> {
    use crate::jupyter::{ExecutionState, JupyterImage, JupyterOutput, OutputKind};

    let removed: Vec<u32> = doc
        .jupyter_outputs
        .iter()
        .filter(|output| output.execution_id.starts_with(STORED_OUTPUT_PREFIX))
        .flat_map(|output| output.images.iter().map(|image| image.id))
        .collect();
    doc.jupyter_outputs
        .retain(|output| !output.execution_id.starts_with(STORED_OUTPUT_PREFIX));

    let Some(nb) = &doc.notebook else {
        return removed;
    };
    static EMPTY: Vec<Value> = Vec::new();
    let cells = nb
        .json
        .get("cells")
        .and_then(Value::as_array)
        .unwrap_or(&EMPTY);

    let text = doc.text().slice(..);
    let spans = scan_cells(text);
    // Skip an implicit leading cell: it has no notebook counterpart.
    let spans = spans
        .iter()
        .skip_while(|span| span.marker_line == span.start_line);

    let mut outputs = Vec::new();
    for (index, (cell, span)) in cells.iter().zip(spans).enumerate() {
        if json_cell_kind(cell) != CellKind::Code {
            continue;
        }
        let Some(stored) = cell.get("outputs").and_then(Value::as_array) else {
            continue;
        };
        if stored.is_empty() {
            continue;
        }
        // Anchor below the cell's last body line (the delimiter for empty cells).
        let anchor_line = span.end_line.saturating_sub(1).max(span.marker_line);
        let mut output = JupyterOutput::new(
            text.line_to_char(anchor_line.min(text.len_lines().saturating_sub(1))),
            format!("{STORED_OUTPUT_PREFIX}{index}"),
            helix_jupyter::KernelId::default(),
        );
        output.state = ExecutionState::Done;

        for entry in stored {
            match entry.get("output_type").and_then(Value::as_str) {
                Some("stream") => {
                    let kind = match entry.get("name").and_then(Value::as_str) {
                        Some("stderr") => OutputKind::Stderr,
                        _ => OutputKind::Stdout,
                    };
                    output.push_text(&json_text(entry.get("text").unwrap_or(&Value::Null)), kind);
                }
                Some("execute_result") | Some("display_data") => {
                    if let Some(data) = entry.get("data") {
                        let plain = json_text(data.get("text/plain").unwrap_or(&Value::Null));
                        let png = json_text(data.get("image/png").unwrap_or(&Value::Null));
                        if let Some(image) = (!png.is_empty())
                            .then(|| {
                                JupyterImage::from_png_base64(
                                    alloc_image_id(),
                                    png.trim().to_string(),
                                )
                            })
                            .flatten()
                        {
                            output.images.push(image);
                        } else if !plain.is_empty() {
                            output.push_text(&plain, OutputKind::Result);
                        }
                    }
                }
                Some("error") => {
                    let traceback = entry
                        .get("traceback")
                        .and_then(Value::as_array)
                        .map(|lines| {
                            lines
                                .iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .unwrap_or_default();
                    if !traceback.is_empty() {
                        output
                            .push_text(&crate::jupyter::strip_ansi(&traceback), OutputKind::Error);
                    }
                }
                _ => {}
            }
        }
        if !output.lines.is_empty() || !output.images.is_empty() {
            outputs.push(output);
        }
    }
    doc.jupyter_outputs.extend(outputs);
    removed
}

/// Serialize one in-editor output block to nbformat output objects:
/// consecutive same-kind lines coalesce into a `stream` / `execute_result` /
/// `error` entry, images become `display_data` PNGs.
fn outputs_to_json(output: &crate::jupyter::JupyterOutput) -> Vec<Value> {
    use crate::jupyter::OutputKind;

    let mut entries = Vec::new();
    let mut run_kind: Option<OutputKind> = None;
    let mut run: Vec<&str> = Vec::new();

    let flush = |kind: Option<OutputKind>, run: &mut Vec<&str>, entries: &mut Vec<Value>| {
        let Some(kind) = kind else { return };
        if run.is_empty() {
            return;
        }
        let entry = match kind {
            OutputKind::Stdout | OutputKind::Stderr => serde_json::json!({
                "output_type": "stream",
                "name": if kind == OutputKind::Stderr { "stderr" } else { "stdout" },
                // Stream output is line-oriented; each line keeps its newline.
                "text": run.iter().map(|line| format!("{line}\n")).collect::<Vec<_>>(),
            }),
            OutputKind::Result => serde_json::json!({
                "output_type": "execute_result",
                "execution_count": null,
                "data": { "text/plain": source_to_json(&run.join("\n")) },
                "metadata": {},
            }),
            OutputKind::Error => serde_json::json!({
                "output_type": "error",
                "ename": "",
                "evalue": "",
                "traceback": run,
            }),
        };
        entries.push(entry);
        run.clear();
    };

    for line in &output.lines {
        if run_kind != Some(line.kind) {
            flush(run_kind, &mut run, &mut entries);
            run_kind = Some(line.kind);
        }
        run.push(&line.text);
    }
    flush(run_kind, &mut run, &mut entries);

    for image in &output.images {
        entries.push(serde_json::json!({
            "output_type": "display_data",
            "data": { "image/png": image.base64 },
            "metadata": {},
        }));
    }
    entries
}

/// The cell containing `line`, if any.
pub fn cell_at_line(cells: &[CellSpan], line: usize) -> Option<&CellSpan> {
    let idx = cells
        .partition_point(|cell| cell.marker_line.min(cell.start_line) <= line)
        .checked_sub(1)?;
    let cell = &cells[idx];
    cell.contains_line(line).then_some(cell)
}

#[cfg(test)]
mod tests {
    use super::*;
    use helix_core::Rope;

    fn scan(text: &str) -> Vec<CellSpan> {
        scan_cells(Rope::from(text).slice(..))
    }

    #[test]
    fn no_markers_means_no_cells() {
        assert!(scan("x = 1\ny = 2\n").is_empty());
        assert!(scan("").is_empty());
    }

    #[test]
    fn splits_on_markers() {
        let cells = scan("# %%\na = 1\n\n# %% [markdown]\n# hello\n# %% [raw]\nraw\n");
        assert_eq!(cells.len(), 3);
        assert_eq!(
            (
                cells[0].kind,
                cells[0].marker_line,
                cells[0].start_line,
                cells[0].end_line
            ),
            (CellKind::Code, 0, 1, 3)
        );
        assert_eq!(
            (cells[1].kind, cells[1].marker_line, cells[1].end_line),
            (CellKind::Markdown, 3, 5)
        );
        assert_eq!(cells[2].kind, CellKind::Raw);
        // Final cell runs to len_lines (trailing newline yields a final empty line).
        assert_eq!(cells[2].end_line, 8);
    }

    #[test]
    fn leading_text_becomes_implicit_code_cell() {
        let cells = scan("import os\n\n# %%\nx = 1\n");
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].kind, CellKind::Code);
        assert_eq!(
            (cells[0].marker_line, cells[0].start_line, cells[0].end_line),
            (0, 0, 2)
        );
        assert_eq!(cells[1].marker_line, 2);
    }

    #[test]
    fn blank_leading_text_is_skipped() {
        let cells = scan("\n\n# %%\nx = 1\n");
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].marker_line, 2);
    }

    #[test]
    fn compact_marker_and_titles() {
        assert_eq!(
            parse_marker(Rope::from("#%%\n").slice(..).line(0)),
            Some(CellKind::Code)
        );
        assert_eq!(
            parse_marker(Rope::from("# %% My title\n").slice(..).line(0)),
            Some(CellKind::Code)
        );
        assert_eq!(
            parse_marker(Rope::from("# %% [markdown] Notes\n").slice(..).line(0)),
            Some(CellKind::Markdown)
        );
        assert_eq!(
            parse_marker(Rope::from("#%% [md]\n").slice(..).line(0)),
            Some(CellKind::Markdown)
        );
    }

    #[test]
    fn non_markers_are_rejected() {
        for line in ["# %%%\n", "#%%-\n", "x = 1 # %%\n", "  # %%\n", "## %%\n"] {
            assert_eq!(
                parse_marker(Rope::from(line).slice(..).line(0)),
                None,
                "{line:?}"
            );
        }
    }

    #[test]
    fn marker_without_trailing_newline() {
        let cells = scan("# %%\nx = 1");
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].end_line, 2);
    }

    #[test]
    fn consecutive_markers_make_empty_cells() {
        let cells = scan("# %%\n# %% [markdown]\n# %%\n");
        assert_eq!(cells.len(), 3);
        assert_eq!((cells[0].start_line, cells[0].end_line), (1, 1));
        assert_eq!((cells[1].start_line, cells[1].end_line), (2, 2));
    }

    fn fixture() -> Value {
        serde_json::json!({
            "cells": [
                {
                    "cell_type": "markdown",
                    "id": "intro",
                    "metadata": {"editable": false},
                    "attachments": {},
                    "source": ["# Title\n", "\n", "Some *text*."]
                },
                {
                    "cell_type": "code",
                    "id": "c1",
                    "execution_count": 3,
                    "metadata": {"tags": ["keep"]},
                    "outputs": [
                        {"output_type": "stream", "name": "stdout", "text": ["hi\n"]}
                    ],
                    "source": ["x = 1\n", "print('hi')"]
                },
                {
                    "cell_type": "raw",
                    "id": "r1",
                    "metadata": {},
                    "source": "raw text"
                }
            ],
            "metadata": {
                "kernelspec": {"name": "python3", "language": "python", "display_name": "Python 3"},
                "language_info": {"name": "python"}
            },
            "nbformat": 4,
            "nbformat_minor": 5
        })
    }

    #[test]
    fn percent_conversion_shape() {
        let text = notebook_to_percent(&fixture());
        assert_eq!(
            text,
            "# %% [markdown]\n# # Title\n#\n# Some *text*.\n\
             # %%\nx = 1\nprint('hi')\n\
             # %% [raw]\n# raw text\n"
        );
    }

    #[test]
    fn unedited_buffer_round_trips_identically() {
        let nb = fixture();
        let text = Rope::from(notebook_to_percent(&nb));
        let back = serialize_notebook(text.slice(..), &nb, &[]).unwrap();
        assert_eq!(back, nb);
    }

    #[test]
    fn edited_cell_keeps_metadata_and_outputs() {
        let nb = fixture();
        let text = notebook_to_percent(&nb).replace("x = 1", "x = 2");
        let back = serialize_notebook(Rope::from(text).slice(..), &nb, &[]).unwrap();
        let cell = &back["cells"][1];
        assert_eq!(cell["id"], "c1");
        assert_eq!(cell["execution_count"], 3);
        assert_eq!(cell["metadata"]["tags"][0], "keep");
        assert_eq!(cell["outputs"][0]["text"][0], "hi\n");
        assert_eq!(json_text(&cell["source"]), "x = 2\nprint('hi')");
    }

    #[test]
    fn new_cell_gets_fresh_identity() {
        let nb = fixture();
        let text = format!("{}# %%\ny = 9\n", notebook_to_percent(&nb));
        let back = serialize_notebook(Rope::from(text).slice(..), &nb, &[]).unwrap();
        let cells = back["cells"].as_array().unwrap();
        assert_eq!(cells.len(), 4);
        let new = &cells[3];
        assert_eq!(new["cell_type"], "code");
        assert_eq!(new["execution_count"], Value::Null);
        assert_eq!(new["outputs"], serde_json::json!([]));
        assert_eq!(new["id"].as_str().unwrap().len(), 8);
        assert_eq!(json_text(&new["source"]), "y = 9");
    }

    #[test]
    fn deleted_cell_is_dropped_and_reorder_matches() {
        let nb = fixture();
        // Reorder: code cell first, markdown second, raw deleted.
        let text = "# %%\nx = 1\nprint('hi')\n# %% [markdown]\n# # Title\n#\n# Some *text*.\n";
        let back = serialize_notebook(Rope::from(text).slice(..), &nb, &[]).unwrap();
        let cells = back["cells"].as_array().unwrap();
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0]["id"], "c1");
        assert_eq!(cells[1]["id"], "intro");
    }

    #[test]
    fn buffer_without_markers_degrades_to_single_code_cell() {
        let nb = fixture();
        let back = serialize_notebook(Rope::from("plain = True\n").slice(..), &nb, &[]).unwrap();
        let cells = back["cells"].as_array().unwrap();
        assert_eq!(cells.len(), 1);
        assert_eq!(json_text(&cells[0]["source"]), "plain = True");
    }

    #[test]
    fn source_trailing_newline_round_trips() {
        // A source that itself ends with a newline survives the percent cycle.
        let nb = serde_json::json!({
            "cells": [{"cell_type": "code", "metadata": {}, "execution_count": null,
                       "outputs": [], "source": ["x = 1\n"]}],
            "metadata": {}, "nbformat": 4, "nbformat_minor": 4
        });
        let text = notebook_to_percent(&nb);
        assert_eq!(text, "# %%\nx = 1\n\n");
        let back = serialize_notebook(Rope::from(text).slice(..), &nb, &[]).unwrap();
        assert_eq!(back, nb);
    }

    #[test]
    fn markdown_comment_encoding_round_trips() {
        for source in [
            "",
            "line",
            "# Heading\n\ntext\n",
            "#bare\n##double",
            "a\n\n\nb",
        ] {
            assert_eq!(
                decode_comment(&encode_comment(source)),
                source,
                "{source:?}"
            );
        }
    }

    #[test]
    fn parse_notebook_rejects_non_notebooks() {
        assert!(parse_notebook("{\"cells\": [], \"nbformat\": 4}").is_some());
        assert!(parse_notebook("{}").is_none());
        assert!(parse_notebook("[1, 2]").is_none());
        assert!(parse_notebook("not json").is_none());
        assert!(parse_notebook("{\"cells\": 5, \"nbformat\": 4}").is_none());
    }

    #[test]
    fn to_json_string_matches_nbformat_style() {
        let out = to_json_string(&serde_json::json!({"a": [1]}));
        assert_eq!(out, "{\n \"a\": [\n  1\n ]\n}\n");
    }

    #[test]
    fn reexecuted_cell_outputs_are_written_back() {
        use crate::jupyter::{JupyterOutput, OutputKind};

        let nb = fixture();
        let text = Rope::from(notebook_to_percent(&nb));
        // The code cell spans lines 4-6 ("# %%", "x = 1", "print('hi')").
        let anchor = text.slice(..).line_to_char(6);
        let mut output = JupyterOutput::new(
            anchor,
            "real-kernel-msg-id".to_string(),
            helix_jupyter::KernelId::default(),
        );
        output.push_text("fresh stdout\n", OutputKind::Stdout);
        output.push_text("42", OutputKind::Result);

        let back = serialize_notebook(text.slice(..), &nb, &[output]).unwrap();
        let outputs = back["cells"][1]["outputs"].as_array().unwrap();
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0]["output_type"], "stream");
        assert_eq!(outputs[0]["text"][0], "fresh stdout\n");
        assert_eq!(outputs[1]["output_type"], "execute_result");
        assert_eq!(outputs[1]["data"]["text/plain"][0], "42");
        assert_eq!(back["cells"][1]["execution_count"], Value::Null);
        // Identity is still preserved.
        assert_eq!(back["cells"][1]["id"], "c1");
    }

    #[test]
    fn stored_output_blocks_do_not_overwrite_outputs() {
        use crate::jupyter::JupyterOutput;

        let nb = fixture();
        let text = Rope::from(notebook_to_percent(&nb));
        let anchor = text.slice(..).line_to_char(6);
        // A block loaded from the file itself must not count as re-execution.
        let mut output = JupyterOutput::new(
            anchor,
            format!("{STORED_OUTPUT_PREFIX}1"),
            helix_jupyter::KernelId::default(),
        );
        output.push_text("hi\n", crate::jupyter::OutputKind::Stdout);

        let back = serialize_notebook(text.slice(..), &nb, &[output]).unwrap();
        assert_eq!(back, nb);
    }

    #[test]
    fn load_outputs_populates_stored_blocks() {
        use crate::jupyter::{ExecutionState, OutputKind};
        use std::sync::Arc;

        // Canonical 1×1 PNG (same as the jupyter module tests).
        const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

        let mut nb = fixture();
        nb["cells"][1]["outputs"].as_array_mut().unwrap().extend([
            serde_json::json!({
                "output_type": "display_data",
                "data": {"image/png": PNG_1X1, "text/plain": ["<Figure>"]},
                "metadata": {},
            }),
            serde_json::json!({
                "output_type": "error", "ename": "E", "evalue": "boom",
                "traceback": ["\u{1b}[31mTraceback\u{1b}[0m", "boom"],
            }),
        ]);

        let mut doc = crate::Document::from(
            Rope::from(notebook_to_percent(&nb)),
            None,
            Arc::new(arc_swap::ArcSwap::new(Arc::new(
                crate::editor::Config::default(),
            ))),
            Arc::new(arc_swap::ArcSwap::from_pointee(
                helix_core::syntax::Loader::default(),
            )),
        );
        doc.notebook = Some(NotebookFile { json: nb });

        let mut next = 0u32;
        let mut alloc = || {
            next += 1;
            next
        };
        load_outputs(&mut doc, &mut alloc);

        assert_eq!(doc.jupyter_outputs.len(), 1);
        let output = &doc.jupyter_outputs[0];
        assert_eq!(output.execution_id, format!("{STORED_OUTPUT_PREFIX}1"));
        assert_eq!(output.state, ExecutionState::Done);
        // Anchored below the code cell's last body line (`print('hi')`).
        assert_eq!(doc.text().char_to_line(output.anchor), 6);
        let kinds: Vec<_> = output
            .lines
            .iter()
            .map(|l| (l.kind, l.text.as_str()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (OutputKind::Stdout, "hi"),
                (OutputKind::Error, "Traceback"),
                (OutputKind::Error, "boom"),
            ]
        );
        assert_eq!(output.images.len(), 1);
        assert_eq!(output.images[0].id, 1);

        // Re-loading replaces the stored block and reports the dropped image.
        let removed = load_outputs(&mut doc, &mut alloc);
        assert_eq!(removed, vec![1]);
        assert_eq!(doc.jupyter_outputs.len(), 1);
        assert_eq!(doc.jupyter_outputs[0].images[0].id, 2);
    }

    #[test]
    fn match_cells_in_place_edits() {
        let retained = fixture()["cells"].as_array().unwrap().clone();
        // All three edited in place: positional pass keeps the pairing.
        let buffer = vec![
            (CellKind::Markdown, "changed".to_string()),
            (CellKind::Code, "changed too".to_string()),
            (CellKind::Raw, "also changed".to_string()),
        ];
        assert_eq!(
            match_cells(&buffer, &retained),
            vec![Some(0), Some(1), Some(2)]
        );
    }

    #[test]
    fn match_cells_insert_between_exact_matches() {
        let retained = fixture()["cells"].as_array().unwrap().clone();
        let buffer = vec![
            (CellKind::Markdown, "# Title\n\nSome *text*.".to_string()),
            (CellKind::Code, "inserted = 1".to_string()),
            (CellKind::Code, "x = 1\nprint('hi')".to_string()),
            (CellKind::Raw, "raw text".to_string()),
        ];
        // The inserted code cell must not steal the exact match of cell 1.
        assert_eq!(
            match_cells(&buffer, &retained),
            vec![Some(0), None, Some(1), Some(2)]
        );
    }

    #[test]
    fn cell_lookup_by_line() {
        let cells = scan("import os\n# %%\na = 1\nb = 2\n# %% [markdown]\n# text\n");
        assert_eq!(cell_at_line(&cells, 0).unwrap().kind, CellKind::Code);
        assert_eq!(cell_at_line(&cells, 0).unwrap().marker_line, 0);
        assert_eq!(cell_at_line(&cells, 1).unwrap().marker_line, 1);
        assert_eq!(cell_at_line(&cells, 3).unwrap().marker_line, 1);
        assert_eq!(cell_at_line(&cells, 4).unwrap().kind, CellKind::Markdown);
        assert_eq!(cell_at_line(&cells, 99), None);
        assert_eq!(cell_at_line(&[], 0), None);
    }
}
