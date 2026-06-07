use std::collections::HashMap;

use helix_core::Position;
use helix_view::jupyter::{rendered_lines, ImagePlacement, JupyterOutput, OutputKind};
use helix_view::theme::Style;
use helix_view::{Document, Theme};

use crate::ui::document::{LinePos, TextRenderer};
use crate::ui::text_decorations::{kitty, Decoration};

struct Styles {
    output: Style,
    error: Style,
}

impl Styles {
    fn new(theme: &Theme) -> Self {
        // Fall back to inlay-hint styling for output, and the diagnostic error
        // style for stderr/tracebacks, when no dedicated keys are themed.
        let output = theme
            .try_get("ui.virtual.jupyter.output")
            .unwrap_or_else(|| {
                theme
                    .try_get("ui.virtual.inlay-hint")
                    .unwrap_or_else(|| theme.get("ui.virtual"))
            });
        let error = theme
            .try_get("ui.virtual.jupyter.error")
            .unwrap_or_else(|| theme.get("error"));
        Self { output, error }
    }

    fn style(&self, kind: OutputKind) -> Style {
        match kind {
            OutputKind::Stdout | OutputKind::Result => self.output,
            OutputKind::Stderr | OutputKind::Error => self.error,
        }
    }
}

/// Draws Jupyter execution output into the virtual lines reserved by
/// `helix_view::jupyter::JupyterLineAnnotation`. Modeled on
/// [`super::diagnostics::InlineDiagnostics`].
pub struct JupyterOutputs<'a> {
    by_line: HashMap<usize, Vec<&'a JupyterOutput>>,
    styles: Styles,
    max_lines: usize,
}

impl<'a> JupyterOutputs<'a> {
    pub fn new(doc: &'a Document, theme: &Theme, max_lines: usize) -> Self {
        let text = doc.text();
        let len = text.len_chars();
        let mut by_line: HashMap<usize, Vec<&'a JupyterOutput>> = HashMap::new();
        for output in &doc.jupyter_outputs {
            let line = text.char_to_line(output.anchor.min(len));
            by_line.entry(line).or_default().push(output);
        }
        Self {
            by_line,
            styles: Styles::new(theme),
            max_lines,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.by_line.is_empty()
    }
}

impl Decoration for JupyterOutputs<'_> {
    fn render_virt_lines(
        &mut self,
        renderer: &mut TextRenderer,
        pos: LinePos,
        virt_off: Position,
    ) -> Position {
        let Some(outputs) = self.by_line.get(&pos.doc_line) else {
            return Position::new(0, 0);
        };

        let start_row = pos.visual_line + virt_off.row as u16;
        let mut row = start_row;
        let x = renderer.viewport.x;
        let width = renderer.viewport.width as usize;

        for output in outputs {
            for line in rendered_lines(output, self.max_lines) {
                let style = self.styles.style(line.kind);
                renderer.set_string_truncated(x, row, &line.text, width, |_| style, true, false);
                row += 1;
            }
            for image in &output.images {
                match image.placement {
                    ImagePlacement::Kitty { rows, cols } => {
                        let id_style = kitty::id_style(image.id);
                        let cols = cols.min(kitty::MAX_CELLS).min(width as u16);
                        for r in 0..rows.min(kitty::MAX_CELLS) {
                            for c in 0..cols {
                                if let Some(symbol) = kitty::placeholder_cell(r, c) {
                                    renderer.set_cell(x + c, row, &symbol, id_style);
                                }
                            }
                            row += 1;
                        }
                    }
                    ImagePlacement::Fallback => {
                        let label = format!("[image {}×{}]", image.width_px, image.height_px);
                        let style = self.styles.style(OutputKind::Result);
                        renderer.set_string_truncated(
                            x,
                            row,
                            &label,
                            width,
                            |_| style,
                            true,
                            false,
                        );
                        row += 1;
                    }
                    ImagePlacement::Pending => {}
                }
            }
        }

        Position::new((row - start_row) as usize, 0)
    }
}
