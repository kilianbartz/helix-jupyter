# Spell checking

This fork ships **`helix-spell-lsp`**, a small language server that spell-checks
**LaTeX, Markdown and Typst** prose. It is tree-sitter aware: it checks the words
you actually wrote and leaves `\commands`, math, code, links, and URLs alone.
Unknown words are reported as diagnostics, and code actions let you fix a typo or
teach the dictionary a new word.

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
- **The LaTeX and Typst grammar sources**, used to compile the server. They are
  vendored under the repo's `runtime/grammars/sources/{latex,typst}` (populated by
  the grammar fetch step). If you have already built the editor's grammars you have
  them; otherwise run `hx --grammar fetch` (or `cargo run -- --grammar fetch`) from
  the repo root, or point `HELIX_SPELL_LATEX_SRC` / `HELIX_SPELL_TYPST_SRC` at a
  grammar checkout's `src` directory (each containing `parser.c` and `scanner.c`).
  (Markdown uses the published `tree-sitter-md` crate and needs no vendored source.)

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
- **Typst** — only markup prose is checked. Code mode (`#let`, `#set`, `#show`,
  function calls, identifiers, strings, numbers), math (`$…$`), raw spans and
  blocks (`` `…` ``, ```` ```…``` ````), comments, labels (`<…>`), references
  (`@…`), and URLs are all skipped. Heading text, `*strong*`/`_emph_` bodies, list
  and term items, and markup *content* passed to functions
  (e.g. `#figure(caption: [checked prose])`) are checked.

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

| Key                   | Default        | Meaning                                                                  |
| --------------------- | -------------- | ------------------------------------------------------------------------ |
| `dictionary`          | `"en_US"`      | Dictionary name to look up in the standard directories.                  |
| `dictionaries`        | `[]`           | Several dictionary names to load together (mixed mode); overrides `dictionary`. |
| `language`            | —              | Restrict checking to this single dictionary (see [Multiple languages](#multiple-languages)). |
| `aff-path`            | —              | Explicit path to a `.aff` file (overrides `dictionary`).                 |
| `dic-path`            | —              | Explicit path to a `.dic` file (overrides `dictionary`).                 |
| `project-dict-file`   | `".spell.dic"` | Project word-list file name, relative to the workspace root.             |
| `project-config-file` | `".spell.toml"`| Per-project config file name, relative to the workspace root.            |
| `severity`            | `"info"`       | Diagnostic severity: `error` \| `warning` \| `info` \| `hint`.           |
| `ignore-uppercase`    | `true`         | Skip all-caps acronyms.                                                  |
| `max-suggestions`     | `5`            | Max replacement suggestions offered per misspelling.                     |

The server is registered for LaTeX, Markdown and Typst with
`only-features = ["diagnostics", "code-action"]`, so it never competes with
`texlab`/`marksman`/`tinymist` for completion, formatting, or navigation.

## Multiple languages

List several dictionaries to spell-check multilingual documents:

```toml
[language-server.helix-spell]
command = "helix-spell-lsp"
config = { dictionaries = ["en_US", "de_DE"] }
```

This is **mixed mode** (the default whenever more than one dictionary is
configured): a word is accepted if *any* loaded dictionary knows it, so English
and German can coexist in the same document. Replacement suggestions are merged
from all dictionaries (each dictionary's best suggestion first, deduplicated,
capped at `max-suggestions`). An explicit `aff-path`/`dic-path` pair counts as
one more dictionary, named after the `.dic` file.

### Per-project language selection

A project can override the language setup with a small **`.spell.toml`** file at
the workspace root (next to `.spell.dic`; the file name is configurable via
`project-config-file`):

```toml
# .spell.toml — restrict this project to a single language:
language = "de_DE"

# or pick which dictionaries to mix (default: the set from languages.toml):
# dictionaries = ["en_US", "de_DE"]
```

Fields set here override their `languages.toml` counterparts field by field.
`language` switches to **single-language mode**: only that dictionary is loaded
and checked against (it does not need to appear in `dictionaries` as long as it
resolves in the standard directories). If it cannot be found, the server reports
an error and checks nothing rather than silently falling back to the wrong
language. The `language` key also works globally in `languages.toml` as a
machine-wide default.

The project and personal word lists ([Dictionaries you can add
to](#dictionaries-you-can-add-to)) are language-agnostic and apply in every mode.

`.spell.toml` is read once at server startup — after editing it, run
`:lsp-restart` to apply the change. On startup the server logs which
dictionaries were loaded and the active mode (e.g.
`helix-spell-lsp ready (dictionaries: en_US, de_DE — mixed mode)`).

## Limitations

- **English by default.** Switch or mix languages with `dictionary`,
  `dictionaries`, or a project `.spell.toml` (see
  [Multiple languages](#multiple-languages)); there is no per-document switching.
- **Mixed mode accepts a word that is valid in *any* configured language**, so
  cross-language false negatives are possible (e.g. a German word in an English
  sentence is not flagged). Use `language` to restrict a project to one language.
- `.spell.toml` is only read at startup; run `:lsp-restart` after changing it.
- **Spelling only** — no grammar/style checking (use `ltex`/`harper` for that).
- **Suggestions are best-effort.** `zspell`'s suggestion engine is edit-distance
  based and may miss transpositions (e.g. it will not always suggest *word* for
  *wrod*). Adding to the dictionary and manual correction always work.
- The whole document is re-parsed on every change. This is fine for the document
  sizes this targets; very large files are not optimized.
- `\href{url}{text}` is skipped in full, so the visible link text there is not
  checked (it usually is in plain `[text](url)` Markdown links).
