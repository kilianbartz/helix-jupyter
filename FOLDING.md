# Code folding

This fork adds **code folding**: collapsing a function or class to just its
signature line so large files are easier to navigate. A folded block is rendered
as its signature line, underlined and followed by a `…` marker; everything below
it (down to the end of the block) is hidden until you unfold it.

```text
def transform(rows):…          ← folded (underlined, with a trailing …)
def main():
    transform(load())
```

Folding is driven by tree-sitter, so it works for any language that ships a
`textobjects.scm` with `function`/`class` captures (this includes Python, the
language the REPL feature targets).

## Commands

| Typed command  | Static command | Description                                   |
| -------------- | -------------- | --------------------------------------------- |
| `:fold`        | `fold_close`   | Fold the function/class at the cursor.        |
| `:unfold`      | `fold_open`    | Unfold the block at the cursor.               |
| —              | `fold_toggle`  | Fold if open, unfold if already folded.       |
| `:fold-all`    | `fold_all`     | Fold every function and class in the buffer.  |
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

## Behavior and limitations

- Folding operates on whole lines: the first (signature) line of the block stays
  visible and every following line of the block is concealed.
- Folds follow edits — inserting or deleting text above or inside a fold keeps it
  anchored, and a fold is dropped automatically if its body is deleted.
- Vertical cursor movement, scrolling, and the line-number gutter all skip folded
  lines. Horizontal movement (`h`/`l`) and `goto`/search into a concealed line do
  not auto-unfold; use `:unfold` / `zo` to reveal it.
- Folds are per-document and are not persisted across editor sessions.
- Blocks with multi-line signatures collapse onto their first line only.
