//! Byte-offset <-> LSP `Position` conversion.
//!
//! LSP positions are `(line, character)` where `character` counts UTF-16 code
//! units, while we slice the document by byte offset. This builds a per-document
//! line index once and converts in both directions: byte -> position to report
//! diagnostics, and position -> byte to turn an incoming selection range into a
//! slice of the document.

use tower_lsp::lsp_types::{Position, Range};

/// Maps byte offsets within a document to LSP positions and back.
pub struct LineIndex<'a> {
    text: &'a str,
    /// Byte offset of the first character of each line.
    line_starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    pub fn new(text: &'a str) -> Self {
        let mut line_starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Self { text, line_starts }
    }

    /// Convert a byte offset into an LSP [`Position`].
    pub fn position(&self, byte: usize) -> Position {
        let line = match self.line_starts.binary_search(&byte) {
            Ok(line) => line,
            Err(next) => next - 1,
        };
        let line_start = self.line_starts[line];
        let col = self.text[line_start..byte]
            .chars()
            .map(char::len_utf16)
            .sum::<usize>();
        Position::new(line as u32, col as u32)
    }

    /// Convert a byte range into an LSP [`Range`].
    pub fn range(&self, start: usize, end: usize) -> Range {
        Range::new(self.position(start), self.position(end))
    }

    /// Convert an LSP [`Position`] back into a byte offset, clamped to the
    /// document. `character` is interpreted as UTF-16 code units per the spec.
    pub fn offset(&self, pos: Position) -> usize {
        let line = pos.line as usize;
        let Some(&line_start) = self.line_starts.get(line) else {
            return self.text.len();
        };
        let line_end = self
            .line_starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.text.len());

        let mut utf16 = 0usize;
        for (rel, ch) in self.text[line_start..line_end].char_indices() {
            if utf16 >= pos.character as usize {
                return line_start + rel;
            }
            utf16 += ch.len_utf16();
        }
        line_end
    }

    /// Convert an LSP [`Range`] into a `(start, end)` byte pair.
    pub fn byte_range(&self, range: Range) -> (usize, usize) {
        (self.offset(range.start), self.offset(range.end))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_positions() {
        let idx = LineIndex::new("abc\ndef\n");
        assert_eq!(idx.position(0), Position::new(0, 0));
        assert_eq!(idx.position(4), Position::new(1, 0));
        assert_eq!(idx.position(6), Position::new(1, 2));
    }

    #[test]
    fn position_round_trips_to_offset() {
        let text = "hello world\nsecond line\n";
        let idx = LineIndex::new(text);
        for byte in [0, 5, 11, 12, 18, text.len()] {
            let pos = idx.position(byte);
            assert_eq!(idx.offset(pos), byte, "byte {byte}");
        }
    }

    #[test]
    fn utf16_columns_and_offsets() {
        // "é" is 2 bytes / 1 UTF-16 unit; "𝐀" is 4 bytes / 2 UTF-16 units.
        let text = "é𝐀x";
        let idx = LineIndex::new(text);
        let bytes = "é𝐀".len();
        let pos = idx.position(bytes);
        assert_eq!(pos, Position::new(0, 3));
        assert_eq!(idx.offset(pos), bytes);
    }

    #[test]
    fn byte_range_slices_selection() {
        let text = "alpha beta gamma";
        let idx = LineIndex::new(text);
        let range = Range::new(Position::new(0, 6), Position::new(0, 10));
        let (s, e) = idx.byte_range(range);
        assert_eq!(&text[s..e], "beta");
    }
}
