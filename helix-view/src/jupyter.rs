//! Per-document Jupyter execution output state.
//!
//! Output blocks are anchored to a char index in the document so they survive
//! edits (remapped via `ChangeSet::update_positions` in `Document::apply`, the
//! same mechanism used for diagnostics). They are rendered as virtual lines
//! below the anchor line by the inline-output decoration in helix-term.

use std::collections::HashMap;

use helix_core::text_annotations::LineAnnotation;
use helix_core::Position;
use helix_jupyter::KernelId;

use crate::Document;

/// Strip ANSI/CSI escape sequences from kernel output (tracebacks are colorized).
pub fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // CSI: ESC '[' ... final byte in 0x40..=0x7e
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            } else {
                // Other escape (e.g. ESC followed by a single char); skip one.
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The stream a line of output came from, used for styling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    Stdout,
    Stderr,
    /// The value of the last expression (`execute_result` / `display_data`).
    Result,
    /// A traceback / error.
    Error,
}

#[derive(Debug, Clone)]
pub struct OutputLine {
    pub text: String,
    pub kind: OutputKind,
}

/// How an image is laid out below the evaluated code, decided by the renderer
/// once the terminal's capabilities and cell size are known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImagePlacement {
    /// Not yet processed by the renderer (no terminal info available yet).
    Pending,
    /// Transmitted to a terminal supporting the kitty graphics protocol; the
    /// image occupies `rows`×`cols` cells, filled with Unicode placeholder cells.
    Kitty { rows: u16, cols: u16 },
    /// The terminal can't render graphics; a one-line text placeholder is shown.
    Fallback,
}

/// A bitmap (currently PNG) produced by an execution, rendered inline as graphics
/// on capable terminals via the kitty Unicode-placeholder protocol.
#[derive(Debug, Clone)]
pub struct JupyterImage {
    /// kitty image id, encoded in the foreground color of each placeholder cell.
    pub id: u32,
    /// base64-encoded PNG, transmitted verbatim to the terminal (kitty `f=100`).
    pub base64: String,
    pub width_px: u32,
    pub height_px: u32,
    pub placement: ImagePlacement,
    /// Whether the image bytes have already been sent to the terminal.
    pub transmitted: bool,
}

impl JupyterImage {
    /// Build an image from a base64 PNG, reading its pixel dimensions from the
    /// PNG header. Returns `None` if the data isn't a PNG we can size.
    pub fn from_png_base64(id: u32, base64: String) -> Option<Self> {
        let (width_px, height_px) = png_dimensions(&base64)?;
        Some(Self {
            id,
            base64,
            width_px,
            height_px,
            placement: ImagePlacement::Pending,
            transmitted: false,
        })
    }

    /// Number of virtual rows this image reserves below the evaluated code. Must
    /// agree between the space-reserving annotation and the drawing decoration.
    pub fn reserved_rows(&self) -> usize {
        match self.placement {
            ImagePlacement::Kitty { rows, .. } => rows as usize,
            ImagePlacement::Fallback => 1,
            ImagePlacement::Pending => 0,
        }
    }
}

/// Total virtual rows reserved by all images in an output block.
pub fn image_rows(output: &JupyterOutput) -> usize {
    output.images.iter().map(|img| img.reserved_rows()).sum()
}

/// Read `(width, height)` in pixels from the IHDR chunk of a base64-encoded PNG.
/// Only the 24-byte prefix is decoded, so this is cheap regardless of image size.
fn png_dimensions(base64: &str) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    let bytes = decode_base64_prefix(base64, 24);
    if bytes.len() < 24 || bytes[0..8] != SIGNATURE || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    Some((width, height))
}

/// Decode up to `max_bytes` of standard base64, skipping any non-alphabet bytes
/// (padding, embedded newlines). Enough to read a PNG header without a base64 dep.
fn decode_base64_prefix(input: &str, max_bytes: usize) -> Vec<u8> {
    fn sextet(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(max_bytes);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for &byte in input.as_bytes() {
        let Some(value) = sextet(byte) else { continue };
        acc = (acc << 6) | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            if out.len() >= max_bytes {
                break;
            }
        }
    }
    out
}

/// Lifecycle of an execution, derived from iopub `status` messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionState {
    Running,
    Done,
    Error,
}

/// One evaluated selection and its accumulated output.
#[derive(Debug, Clone)]
pub struct JupyterOutput {
    /// Char index the output block is anchored to; virtual lines render below
    /// the line containing this index.
    pub anchor: usize,
    /// The execution request `msg_id`. Equals the `parent_header.msg_id` carried
    /// by every result message, used to route incoming output to this block.
    pub execution_id: String,
    pub kernel: KernelId,
    pub lines: Vec<OutputLine>,
    /// Images (plots etc.) produced by the execution, rendered below `lines`.
    pub images: Vec<JupyterImage>,
    pub state: ExecutionState,
    /// The `msg_id` of the silent variable-introspection follow-up execution, if any.
    pub inspect_execution_id: Option<String>,
    /// Accumulated stdout of the introspection execution (a JSON object).
    pub inspect_buffer: String,
    /// Parsed variable name → `repr(value)` pairs from the selection.
    pub variables: Vec<(String, String)>,
}

impl JupyterOutput {
    pub fn new(anchor: usize, execution_id: String, kernel: KernelId) -> Self {
        Self {
            anchor,
            execution_id,
            kernel,
            lines: Vec::new(),
            images: Vec::new(),
            state: ExecutionState::Running,
            inspect_execution_id: None,
            inspect_buffer: String::new(),
            variables: Vec::new(),
        }
    }

    /// Parse the accumulated introspection JSON (`{name: repr}`) into `variables`.
    pub fn parse_inspect_buffer(&mut self) {
        let trimmed = self.inspect_buffer.trim();
        if trimmed.is_empty() {
            return;
        }
        if let Ok(serde_json::Value::Object(map)) =
            serde_json::from_str::<serde_json::Value>(trimmed)
        {
            self.variables = map
                .into_iter()
                .map(|(name, value)| {
                    let value = match value {
                        serde_json::Value::String(s) => s,
                        other => other.to_string(),
                    };
                    (name, value)
                })
                .collect();
            self.variables.sort_by(|a, b| a.0.cmp(&b.0));
        }
    }

    /// Append text from a stream/result, splitting on newlines. A chunk that
    /// splits mid-line (streamed stdout) continues the previous line when it has
    /// the same kind.
    pub fn push_text(&mut self, text: &str, kind: OutputKind) {
        let mut segments = text.split('\n');
        if let Some(first) = segments.next() {
            match self.lines.last_mut() {
                Some(last) if last.kind == kind => last.text.push_str(first),
                _ => self.lines.push(OutputLine {
                    text: first.to_string(),
                    kind,
                }),
            }
        }
        for segment in segments {
            self.lines.push(OutputLine {
                text: segment.to_string(),
                kind,
            });
        }
        // A trailing newline produces a final empty segment; drop it so we don't
        // render a blank line at the end of the block.
        if let Some(last) = self.lines.last() {
            if last.text.is_empty() {
                self.lines.pop();
            }
        }
    }
}

/// A single line to render below the evaluated code.
pub struct RenderedLine {
    pub text: String,
    pub kind: OutputKind,
}

/// Compute the exact lines rendered for an output block, capped at `max`. Used
/// by *both* the space-reserving [`JupyterLineAnnotation`] and the drawing
/// decoration so they always agree on the number of virtual rows.
pub fn rendered_lines(output: &JupyterOutput, max: usize) -> Vec<RenderedLine> {
    let mut lines = Vec::new();
    if output.lines.is_empty() {
        if output.state == ExecutionState::Running {
            lines.push(RenderedLine {
                text: "● running…".to_string(),
                kind: OutputKind::Stdout,
            });
        }
        return lines;
    }
    let max = max.max(1);
    for line in output.lines.iter().take(max) {
        lines.push(RenderedLine {
            text: line.text.clone(),
            kind: line.kind,
        });
    }
    if output.lines.len() > max {
        lines.push(RenderedLine {
            text: format!("… {} more lines", output.lines.len() - max),
            kind: OutputKind::Stdout,
        });
    }
    lines
}

/// [`LineAnnotation`] that reserves virtual line space below each evaluated line
/// for its Jupyter output block. The drawing happens in the helix-term
/// `JupyterOutput` decoration.
pub struct JupyterLineAnnotation {
    rows_by_line: HashMap<usize, usize>,
}

impl JupyterLineAnnotation {
    pub fn new(doc: &Document, max_output_lines: usize) -> Box<dyn LineAnnotation> {
        let text = doc.text();
        let len = text.len_chars();
        let mut rows_by_line: HashMap<usize, usize> = HashMap::new();
        for output in &doc.jupyter_outputs {
            let line = text.char_to_line(output.anchor.min(len));
            let rows = rendered_lines(output, max_output_lines).len() + image_rows(output);
            *rows_by_line.entry(line).or_insert(0) += rows;
        }
        Box::new(Self { rows_by_line })
    }
}

impl LineAnnotation for JupyterLineAnnotation {
    fn insert_virtual_lines(
        &mut self,
        _line_end_char_idx: usize,
        _line_end_visual_pos: Position,
        doc_line: usize,
    ) -> Position {
        Position::new(self.rows_by_line.get(&doc_line).copied().unwrap_or(0), 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Canonical 1×1 PNG.
    const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

    #[test]
    fn reads_png_dimensions() {
        assert_eq!(png_dimensions(PNG_1X1), Some((1, 1)));
    }

    #[test]
    fn tolerates_embedded_newlines() {
        let wrapped = format!("{}\n{}", &PNG_1X1[..20], &PNG_1X1[20..]);
        assert_eq!(png_dimensions(&wrapped), Some((1, 1)));
    }

    #[test]
    fn rejects_non_png() {
        assert_eq!(png_dimensions("bm90IGEgcG5n"), None); // "not a png"
        assert_eq!(png_dimensions(""), None);
    }

    #[test]
    fn from_png_base64_sets_pending_placement() {
        let image = JupyterImage::from_png_base64(7, PNG_1X1.to_string()).unwrap();
        assert_eq!((image.width_px, image.height_px), (1, 1));
        assert_eq!(image.placement, ImagePlacement::Pending);
        assert!(!image.transmitted);
        assert_eq!(image.reserved_rows(), 0);
    }
}
