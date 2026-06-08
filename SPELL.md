# Spell checking

This fork ships **`helix-spell-lsp`**, a small language server that spell-checks
**LaTeX and Markdown** prose. It is tree-sitter aware: it checks the words you
actually wrote and leaves `\commands`, math, code, links, and URLs alone. Unknown
words are reported as diagnostics, and code actions let you fix a typo or teach
the dictionary a new word.

```text
\section{Introduktion}          ← "Introduktion" flagged
This is a paragrahp of text.     ← "paragrahp" flagged
\cite{einstein1905}              ← citation key ignored
$E = mc^2$                       ← math ignored
\url{http://misspeld.example}    ← URL ignored
```

The server lives in its own crate (`helix-spell-lsp/`) and is wired into the
editor through `languages.toml` like any other language server — there are no new
`:commands`.

## Prerequisites

- **A Hunspell dictionary.** By default the server loads `en_US` from the standard
  system locations (`/usr/share/hunspell`, `/usr/share/myspell`, …). On most Linux
  distributions install `hunspell-en_US` (or `hunspell-en`). Any Hunspell
  `.aff`/`.dic` pair works; see [Configuration](#configuration) to choose another.
- **The `helix-spell-lsp` binary on `PATH`** (see [Building](#building)).
- **The LaTeX grammar source**, used to compile the server. It is vendored under
  the repo's `runtime/grammars/sources/latex` (populated by the grammar fetch
  step). If you have already built the editor's grammars you have it; otherwise run
  `hx --grammar fetch` (or `cargo run -- --grammar fetch`) from the repo root, or
  point `HELIX_SPELL_LATEX_SRC` at a `tree-sitter-latex` checkout's `src` directory.

## Building

```sh
# from the repo root
cargo install --path helix-spell-lsp --locked     # installs `helix-spell-lsp` into ~/.cargo/bin
```

Or, for development, build it and point the `languages.toml` `command` at the
binary by absolute path:

```sh
cargo build --release --manifest-path helix-spell-lsp/Cargo.toml
# binary at helix-spell-lsp/target/release/helix-spell-lsp
```

## How it decides what to check

The server parses each document with tree-sitter and only spell-checks prose:

- **LaTeX** — command control sequences (`\section`, `\textbf`, …), math
  (`$…$`, `\[…\]`, equation environments), comments, verbatim/listing/minted/code
  environments, and the arguments of reference/citation/include/url commands
  (`\cite`, `\ref`, `\label`, `\input`, `\usepackage`, `\url`, `\href`, `\cref`, …)
  are all skipped. The body of formatting commands such as `\textbf{…}` and the
  titles of sectioning commands *are* checked.
- **Markdown** — fenced and indented code blocks, inline code spans, link/image
  destinations, autolinks, raw HTML, and YAML/TOML frontmatter are skipped. Heading
  text, emphasis, and link *text* are checked.

Words containing digits (`h2o`, `3rd`) are ignored, and all-caps acronyms
(`NASA`, `HTTP`) are ignored by default (configurable).

## Dictionaries you can add to

Two user word lists are consulted in addition to the base dictionary:

| List         | Location                                            | Use                                  |
| ------------ | --------------------------------------------------- | ------------------------------------ |
| **Project**  | `<workspace-root>/.spell.dic`                       | Project jargon; commit it with the repo. |
| **Personal** | `<config>/helix-spell/personal.dic` (e.g. `~/.config/helix-spell/personal.dic`) | Your personal vocabulary, all projects. |

Each file is plain text, one word per line; lines starting with `#` are comments.
Matching is case-insensitive.

## Code actions

Place the cursor on a flagged word and open code actions (default `<space>a`):

- **Replace with "…"** — apply one of the suggested corrections. (Suggestions are
  best-effort — see [Limitations](#limitations).)
- **Add "word" to project dictionary** — append it to `.spell.dic` at the
  workspace root.
- **Add "word" to personal dictionary** — append it to your personal list.

Adding a word re-checks every open document immediately, so the warning disappears
everywhere at once.

## Configuration

Options are passed through the `config` table of the server entry in
`languages.toml` (forwarded to the server as `initializationOptions`). All are
optional:

```toml
[language-server.helix-spell]
command = "helix-spell-lsp"
config = { dictionary = "en_US", project-dict-file = ".spell.dic", severity = "info", ignore-uppercase = true, max-suggestions = 5 }
```

| Key                 | Default       | Meaning                                                            |
| ------------------- | ------------- | ------------------------------------------------------------------ |
| `dictionary`        | `"en_US"`     | Dictionary name to look up in the standard directories.            |
| `aff-path`          | —             | Explicit path to a `.aff` file (overrides `dictionary`).           |
| `dic-path`          | —             | Explicit path to a `.dic` file (overrides `dictionary`).           |
| `project-dict-file` | `".spell.dic"`| Project word-list file name, relative to the workspace root.       |
| `severity`          | `"info"`      | Diagnostic severity: `error` \| `warning` \| `info` \| `hint`.     |
| `ignore-uppercase`  | `true`        | Skip all-caps acronyms.                                            |
| `max-suggestions`   | `5`           | Max replacement suggestions offered per misspelling.               |

The server is registered for LaTeX and Markdown with
`only-features = ["diagnostics", "code-action"]`, so it never competes with
`texlab`/`marksman` for completion, formatting, or navigation.

## Limitations

- **English by default**, one dictionary at a time. Switch languages with
  `dictionary` (or `aff-path`/`dic-path`); there is no per-document switching.
- **Spelling only** — no grammar/style checking (use `ltex`/`harper` for that).
- **Suggestions are best-effort.** `zspell`'s suggestion engine is edit-distance
  based and may miss transpositions (e.g. it will not always suggest *word* for
  *wrod*). Adding to the dictionary and manual correction always work.
- The whole document is re-parsed on every change. This is fine for the document
  sizes this targets; very large files are not optimized.
- `\href{url}{text}` is skipped in full, so the visible link text there is not
  checked (it usually is in plain `[text](url)` Markdown links).
