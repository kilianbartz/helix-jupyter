//! Byte-offset <-> LSP `Position` conversion.
//!
//! Tree-sitter and our tokenizer both work in byte offsets, but LSP positions
//! are `(line, character)` where `character` counts UTF-16 code units. This
//! module builds a per-document line index once and converts offsets against it.

use tower_lsp::lsp_types::{Position, Range};

/// Maps byte offsets within a document to LSP positions.
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
        // Find the last line whose start is <= byte.
        let line = match self.line_starts.binary_search(&byte) {
            Ok(line) => line,
            Err(next) => next - 1,
        };
        let line_start = self.line_starts[line];
        // UTF-16 code units between the line start and the offset.
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_positions() {
        let idx = LineIndex::new("abc\ndef\n");
        assert_eq!(idx.position(0), Position::new(0, 0));
        assert_eq!(idx.position(2), Position::new(0, 2));
        assert_eq!(idx.position(4), Position::new(1, 0));
        assert_eq!(idx.position(6), Position::new(1, 2));
    }

    #[test]
    fn utf16_columns() {
        // "é" is 2 bytes UTF-8 but 1 UTF-16 unit; "𝐀" is 4 bytes / 2 UTF-16 units.
        let text = "é𝐀x";
        let idx = LineIndex::new(text);
        let bytes = "é𝐀".len(); // byte offset of 'x'
        assert_eq!(idx.position(bytes), Position::new(0, 3));
    }
}
