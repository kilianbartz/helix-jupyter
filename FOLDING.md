# Code folding

This fork adds **code folding**: collapsing a function, class, or multi-line
string to just its first (signature/opening) line so large files are easier to
navigate. A folded block is rendered as its opening line, underlined and
followed by a `…` marker; everything below it (down to the end of the block) is
hidden until you unfold it.

```text
def transform(rows):…          ← folded (underlined, with a trailing …)
def main():
    transform(load())
```

Folding is driven by tree-sitter, so it works for any language that ships a
`textobjects.scm` with `function`/`class` captures (this includes Python, the
language the REPL feature targets). Multi-line strings are folded too — these
are located directly from the syntax tree (`string` nodes), so any grammar that
names its string literals `string` (Python triple-quoted strings and docstrings
included) gets string folding without needing a textobject capture.

## Commands

| Typed command  | Static command | Description                                   |
| -------------- | -------------- | --------------------------------------------- |
| `:fold`        | `fold_close`   | Fold the function/class/string at the cursor. |
| `:unfold`      | `fold_open`    | Unfold the block at the cursor.               |
| —              | `fold_toggle`  | Fold if open, unfold if already folded.       |
| `:fold-all`    | `fold_all`     | Fold every function, class, and string.       |
| `:unfold-all`  | `unfold_all`   | Unfold everything.                            |

`fold_all` folds the outermost blocks: a folded class hides its methods rather
than folding each method separately.

## Default keybindings

The bindings live in the `z` / `Z` (view) menu:

| Key  | Command       |
| ---- | ------------- |
| `za` | `fold_toggle` |
| `zf` | `fold_close`  |
| `zo` | `fold_open`   |
| `zM` | `fold_all`    |
| `zR` | `unfold_all`  |

Bind the static commands to your own keys in `config.toml` if you prefer, e.g.:

```toml
[keys.normal]
"C-y" = "fold_toggle"
```

## Theming

The `…` marker uses the `ui.virtual.fold` theme scope (falling back to nothing
if unset). The signature line is always underlined; if `ui.virtual.fold` sets an
`underline_color`, that color is used for the underline. Example:

```toml
"ui.virtual.fold" = { fg = "gray", underline_color = "gray" }
```

Closed folds are also marked in the gutter with a `◆` diamond on the signature
line. The marker shares the existing `diagnostics` gutter column (so it adds no
width); it shows only when that line has no higher-priority marker
(execution-pause indicator, breakpoint, or diagnostic). The diamond uses the
`ui.gutter.fold` scope, falling back to `ui.virtual.fold` and finally to a white
diamond if neither is themed:

```toml
"ui.gutter.fold" = { fg = "#c9a0ff" }   # light purple diamond
```

## Behavior and limitations

- Folding operates on whole lines: the first (signature) line of the block stays
  visible and every following line of the block is concealed.
- Folds follow edits — inserting or deleting text above or inside a fold keeps it
  anchored, and a fold is dropped automatically if its body is deleted.
- Folds may be nested: a fold whose range fully contains (or is fully contained
  by) an existing fold is allowed, so a LaTeX section can be folded even when its
  subsections are already folded. Unfolding the outer block reveals the inner
  folds still collapsed. Only *partially* overlapping folds are rejected.
- Vertical cursor movement, scrolling, and the line-number gutter all skip folded
  lines. Horizontal movement (`h`/`l`) and `goto`/search into a concealed line do
  not auto-unfold; use `:unfold` / `zo` to reveal it.
- Folds are persisted across sessions in `.helix/folds.toml` at the workspace
  root (the directory that contains `.git`, `.svn`, `.jj`, or `.helix`). Folds
  are restored automatically when a file is opened. If no workspace root is
  found the current working directory is used and `.helix/folds.toml` is
  written there.
- Blocks with multi-line signatures collapse onto their first line only.
