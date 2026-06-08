//! Tree-sitter-driven extraction of spell-checkable words.
//!
//! Strategy (uniform across languages): parse the document, collect the byte
//! ranges of *non-prose* nodes (commands, math, code, URLs, references…) into a
//! merged skip-list, then tokenize the whole document into words and drop any
//! word that overlaps a skipped range. This naturally handles Markdown, whose
//! inline grammar represents plain prose as anonymous tokens (there is no
//! `text` node to collect), while still precisely excluding code spans, link
//! targets, math, etc.

use tree_sitter::{Language as TsLanguage, Node, Parser};
use tree_sitter_language::LanguageFn;

extern "C" {
    fn tree_sitter_latex() -> *const ();
    fn tree_sitter_typst() -> *const ();
}

/// The LaTeX grammar, compiled from C sources by `build.rs`.
fn latex_language() -> TsLanguage {
    let f: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_latex) };
    f.into()
}

/// The Typst grammar, compiled from C sources by `build.rs`.
fn typst_language() -> TsLanguage {
    let f: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_typst) };
    f.into()
}

/// A spell-checkable word together with its location in the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// Languages this server knows how to extract prose from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Latex,
    Markdown,
    Typst,
}

impl Language {
    /// Detect the language from a file extension or an LSP `languageId`.
    pub fn detect(extension_or_id: &str) -> Option<Language> {
        match extension_or_id
            .trim_start_matches('.')
            .to_ascii_lowercase()
            .as_str()
        {
            "tex" | "latex" | "sty" | "cls" | "dtx" | "ins" | "bbx" | "cbx" => {
                Some(Language::Latex)
            }
            "md" | "markdown" | "mdx" | "mkd" | "mkdn" | "mdwn" | "mdown" | "markdn" | "mdtxt"
            | "mdtext" | "livemd" => Some(Language::Markdown),
            "typ" | "typst" => Some(Language::Typst),
            _ => None,
        }
    }
}

/// LaTeX node kinds whose entire subtree is non-prose and must be skipped.
const LATEX_SKIP: &[&str] = &[
    // command tokens and environment name markers
    "command_name",
    "curly_group_command_name",
    "begin",
    "end",
    // comments
    "comment",
    "line_comment",
    "block_comment",
    // math
    "displayed_equation",
    "inline_formula",
    "math_environment",
    "math_delimiter",
    // verbatim / code environments
    "verbatim_environment",
    "minted_environment",
    "listing_environment",
    "pycode_environment",
    "luacode_environment",
    "sageblock_environment",
    "sagesilent_environment",
    "asy_environment",
    "asydef_environment",
    "comment_environment",
    "source_code",
    // references / labels / citations whose arguments are identifiers
    "citation",
    "label_reference",
    "label_reference_range",
    "label_definition",
    "label_number",
    "acronym_reference",
    "glossary_entry_reference",
    "color_reference",
    "color_definition",
    "color_set_definition",
    // \input / \include / \usepackage / graphics / bib … (path-like args)
    "latex_include",
    "package_include",
    "class_include",
    "bibtex_include",
    "biblatex_include",
    "bibstyle_include",
    "graphics_include",
    "svg_include",
    "import_include",
    "inkscape_include",
    "verbatim_include",
    "tikz_library_import",
    // paths
    "path",
    "glob_pattern",
];

/// Generic LaTeX commands whose curly-group argument is a URL / label / path
/// rather than prose. The 0.3.0 grammar parses these as plain `generic_command`s
/// (no dedicated node), so we skip them by command name. The whole command —
/// including any visible text argument of e.g. `\href` — is skipped; link text
/// is short, so the loss of spell-checking there is acceptable.
const LATEX_SKIP_ARG_COMMANDS: &[&str] = &[
    "url",
    "nolinkurl",
    "href",
    "path",
    "lstinline",
    "verb",
    "cref",
    "Cref",
    "cpageref",
    "Cpageref",
    "autoref",
    "nameref",
    "vref",
    "eqref",
    "pageref",
    "cite",
    "citep",
    "citet",
    "ref",
    "label",
];

/// Markdown (block + inline) node kinds to skip.
const MARKDOWN_SKIP: &[&str] = &[
    // block
    "fenced_code_block",
    "indented_code_block",
    "html_block",
    "link_reference_definition",
    "minus_metadata", // YAML frontmatter
    "plus_metadata",  // TOML frontmatter
    // inline
    "code_span",
    "link_destination",
    "link_label",
    "link_title",
    "uri_autolink",
    "email_autolink",
    "html_tag",
    "entity_reference",
    "numeric_character_reference",
    "latex_block",
];

/// Extract the spell-checkable words from `text` for the given language.
pub fn extract(language: Language, text: &str, skip_acronyms: bool) -> Vec<Word> {
    let skip = match language {
        Language::Latex => latex_skip_ranges(text),
        Language::Markdown => markdown_skip_ranges(text),
        Language::Typst => typst_skip_ranges(text),
    };
    let merged = merge_ranges(skip);
    tokenize(text, &merged, skip_acronyms)
}

fn latex_skip_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut parser = Parser::new();
    if parser.set_language(&latex_language()).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(text, None) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    collect_latex_skip(tree.root_node(), text.as_bytes(), &mut out);
    out
}

/// Like [`collect_skip`], but also skips whole `generic_command` subtrees whose
/// command name is in [`LATEX_SKIP_ARG_COMMANDS`] (e.g. `\url{…}`, `\cref{…}`).
fn collect_latex_skip(node: Node, src: &[u8], out: &mut Vec<(usize, usize)>) {
    let kind = node.kind();
    // Structural commands (\section, \cite, \begin, …) appear as anonymous
    // tokens whose kind *is* the literal control sequence. Generic commands use
    // a named `command_name` (in LATEX_SKIP). Either way, never prose.
    if kind.starts_with('\\') || LATEX_SKIP.contains(&kind) {
        out.push((node.start_byte(), node.end_byte()));
        return;
    }
    if node.kind() == "generic_command" {
        if let Some(name) = generic_command_name(node, src) {
            if LATEX_SKIP_ARG_COMMANDS.contains(&name) {
                out.push((node.start_byte(), node.end_byte()));
                return;
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_latex_skip(child, src, out);
    }
}

/// The command name (without the leading backslash) of a `generic_command`.
fn generic_command_name<'a>(node: Node, src: &'a [u8]) -> Option<&'a str> {
    let command = node.child_by_field_name("command")?;
    let text = command.utf8_text(src).ok()?;
    Some(text.trim_start_matches('\\'))
}

fn markdown_skip_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut parser = tree_sitter_md::MarkdownParser::default();
    let Some(tree) = parser.parse(text.as_bytes(), None) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    collect_skip(tree.block_tree().root_node(), MARKDOWN_SKIP, &mut out);
    for inline in tree.inline_trees() {
        collect_skip(inline.root_node(), MARKDOWN_SKIP, &mut out);
    }
    out
}

/// Typst inverts the usual strategy: prose lives exclusively in `text` nodes
/// (markup mode), while code, math, raw blocks, references, labels, URLs and
/// strings use other node kinds — and code blocks can embed markup `content`
/// (`#figure(caption: [prose])`), so a skip-list of code subtrees would wrongly
/// drop that prose. Instead we collect the byte ranges of `text` nodes as an
/// *include* list and skip everything else (the complement). The Typst grammar's
/// `text` rule spans whole prose runs — apostrophes and other punctuation
/// included — so contractions like `don't` survive as a single token.
fn typst_skip_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut parser = Parser::new();
    if parser.set_language(&typst_language()).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(text, None) else {
        return Vec::new();
    };
    let mut include = Vec::new();
    // `quote` is the markup smart-quote node (a lone `'` or `"`). Including it
    // lets an apostrophe between two `text` nodes merge them back together, so
    // contractions like `don't` survive as a single token.
    collect_kind(tree.root_node(), TYPST_PROSE, &mut include);
    complement_ranges(&merge_ranges(include), text.len())
}

/// Typst node kinds that carry (or glue together) prose. See [`typst_skip_ranges`].
const TYPST_PROSE: &[&str] = &["text", "quote"];

/// Recursively record the byte range of every node whose kind is in `kinds`.
/// Matched nodes are not descended into — their range already covers children.
fn collect_kind(node: Node, kinds: &[&str], out: &mut Vec<(usize, usize)>) {
    if kinds.contains(&node.kind()) {
        out.push((node.start_byte(), node.end_byte()));
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_kind(child, kinds, out);
    }
}

/// The complement of a sorted, disjoint `include` list within `[0, total)`:
/// every byte range *not* covered by an include range.
fn complement_ranges(include: &[(usize, usize)], total: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut pos = 0;
    for &(s, e) in include {
        if s > pos {
            out.push((pos, s));
        }
        pos = pos.max(e);
    }
    if pos < total {
        out.push((pos, total));
    }
    out
}

/// Recursively record the byte range of every node whose kind is in `skip`.
/// Skipped nodes are not descended into — their range already covers children.
fn collect_skip(node: Node, skip: &[&str], out: &mut Vec<(usize, usize)>) {
    if skip.contains(&node.kind()) {
        out.push((node.start_byte(), node.end_byte()));
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_skip(child, skip, out);
    }
}

/// Sort and merge a list of (possibly overlapping) byte ranges into a disjoint,
/// ascending list.
fn merge_ranges(mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for (s, e) in ranges {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    merged
}

/// Does `[ws, we)` overlap any interval in the sorted, disjoint `merged` list?
fn overlaps(merged: &[(usize, usize)], ws: usize, we: usize) -> bool {
    // First interval whose end is strictly greater than the word start.
    let idx = merged.partition_point(|&(_, e)| e <= ws);
    matches!(merged.get(idx), Some(&(s, _)) if s < we)
}

/// Split `text` into candidate words, dropping those that overlap a skip range,
/// contain digits, are too short, or (optionally) are all-caps acronyms.
fn tokenize(text: &str, skip: &[(usize, usize)], skip_acronyms: bool) -> Vec<Word> {
    let mut words = Vec::new();
    let bytes = text.as_bytes();
    let mut iter = text.char_indices().peekable();

    while let Some(&(start, c)) = iter.peek() {
        if !is_word_char(c) {
            iter.next();
            continue;
        }
        // Consume a maximal run of word characters.
        let mut end = start;
        let mut has_digit = false;
        while let Some(&(i, ch)) = iter.peek() {
            if is_word_char(ch) {
                has_digit |= ch.is_numeric();
                end = i + ch.len_utf8();
                iter.next();
            } else {
                break;
            }
        }

        if has_digit {
            continue; // identifiers / numbers like "h2o", "3rd", "x86"
        }

        // Trim leading/trailing apostrophes (e.g. quoted 'word').
        let (s, e) = trim_apostrophes(bytes, start, end);
        if e <= s {
            continue;
        }
        let word = &text[s..e];
        if word.chars().count() < 2 {
            continue;
        }
        if overlaps(skip, s, e) {
            continue;
        }
        if skip_acronyms && is_all_uppercase(word) {
            continue;
        }
        words.push(Word {
            start: s,
            end: e,
            text: word.to_string(),
        });
    }
    words
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '\'' || c == '\u{2019}' // straight or curly apostrophe
}

fn trim_apostrophes(bytes: &[u8], mut start: usize, mut end: usize) -> (usize, usize) {
    while start < end && bytes[start] == b'\'' {
        start += 1;
    }
    while end > start && is_apostrophe_at_end(bytes, start, end) {
        end -= apostrophe_len_at_end(bytes, start, end);
    }
    (start, end)
}

fn is_apostrophe_at_end(bytes: &[u8], start: usize, end: usize) -> bool {
    if end == start {
        return false;
    }
    if bytes[end - 1] == b'\'' {
        return true;
    }
    // curly apostrophe ’ = E2 80 99
    end >= start + 3 && bytes[end - 3] == 0xE2 && bytes[end - 2] == 0x80 && bytes[end - 1] == 0x99
}

fn apostrophe_len_at_end(bytes: &[u8], _start: usize, end: usize) -> usize {
    if bytes[end - 1] == b'\'' {
        1
    } else {
        3
    }
}

fn is_all_uppercase(word: &str) -> bool {
    let mut has_alpha = false;
    for c in word.chars() {
        if c.is_alphabetic() {
            has_alpha = true;
            if !c.is_uppercase() {
                return false;
            }
        }
    }
    has_alpha
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words_of(language: Language, src: &str) -> Vec<String> {
        extract(language, src, true)
            .into_iter()
            .map(|w| w.text)
            .collect()
    }

    fn has(words: &[String], word: &str) -> bool {
        words.iter().any(|w| w == word)
    }

    #[test]
    fn latex_skips_commands_and_math() {
        let src = r"\section{Introduction}
        Here is some prose with a typoo.
        \cite{einstein1905} and \ref{eq:main}.
        \[ E = mc^2 \]
        Inline $x_{wrong}$ math.
        % a commentt line
        \textbf{bold wordd}";
        let words = words_of(Language::Latex, src);
        assert!(has(&words, "Introduction"));
        assert!(has(&words, "prose"));
        assert!(has(&words, "typoo"));
        assert!(has(&words, "bold"));
        assert!(has(&words, "wordd"));
        // command names, citation/ref args, math, comments are excluded:
        assert!(!has(&words, "section"));
        assert!(!has(&words, "textbf"));
        assert!(!has(&words, "einstein1905"));
        assert!(!has(&words, "eq"));
        assert!(!has(&words, "main"));
        assert!(!has(&words, "wrong"));
        assert!(!has(&words, "commentt"));
        assert!(!has(&words, "mc"));
    }

    #[test]
    fn markdown_skips_code_and_links() {
        let src = "# Headingg\n\nSome prose with a typoo and `code_span_wrong` here.\n\n```rust\nlet wrongg = 1;\n```\n\n[link text](http://wrongurl.example) and <http://auto.example>.\n";
        let words = words_of(Language::Markdown, src);
        assert!(has(&words, "Headingg"));
        assert!(has(&words, "prose"));
        assert!(has(&words, "typoo"));
        assert!(has(&words, "link"));
        assert!(has(&words, "text"));
        // code span, fenced code, and URLs are excluded:
        assert!(!has(&words, "code"));
        assert!(!has(&words, "span"));
        assert!(!has(&words, "wrongg"));
        assert!(!has(&words, "wrongurl"));
        assert!(!has(&words, "auto"));
        assert!(!has(&words, "http"));
    }

    #[test]
    fn typst_skips_code_math_and_refs() {
        let src = r#"= Headingg

Some prose with a typoo and a contraction don't here.

#let myvariabel = 42
#set text(font: "Arial")

Inline `code_wrong` and a $ x_wrongg $ formula.

A #link("https://wrongurl.example")[visible linkk] and a @badref reference.

// a commentt line

A figure with #figure(caption: [a captionn typo]).

```rust
let wrongg = 1;
```
"#;
        let words = words_of(Language::Typst, src);
        assert!(has(&words, "Headingg"));
        assert!(has(&words, "prose"));
        assert!(has(&words, "typoo"));
        assert!(has(&words, "don't"));
        assert!(has(&words, "visible"));
        assert!(has(&words, "linkk"));
        // markup embedded in a code call argument is still checked:
        assert!(has(&words, "captionn"));
        // code identifiers, strings, math, raw, refs, comments are excluded:
        assert!(!has(&words, "myvariabel"));
        assert!(!has(&words, "Arial"));
        assert!(!has(&words, "code_wrong"));
        assert!(!has(&words, "wrongg"));
        assert!(!has(&words, "wrongurl"));
        assert!(!has(&words, "badref"));
        assert!(!has(&words, "commentt"));
        assert!(!has(&words, "font"));
    }

    #[test]
    fn drops_numbers_and_acronyms() {
        let words = words_of(Language::Markdown, "We have 3rd h2o and NASA stuff.");
        assert!(has(&words, "have"));
        assert!(has(&words, "stuff"));
        assert!(!has(&words, "rd"));
        assert!(!has(&words, "h2o"));
        assert!(!has(&words, "NASA"));
    }
}
