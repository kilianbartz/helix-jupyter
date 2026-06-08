# LaTeX navigation

This fork adds **section-aware navigation** for LaTeX documents: jump between
structural blocks (`\chapter`, `\section`, `\subsection`, etc.) the same way
you jump between functions in code.

The feature is tree-sitter driven, using a custom `@section.around` capture in
`runtime/queries/latex/textobjects.scm`.

## Commands

| Static command              | Description                                                  |
| --------------------------- | ------------------------------------------------------------ |
| `goto_next_section`         | Move to the start of the next section/chapter/paragraph.     |
| `goto_prev_section`         | Move to the start of the previous section/chapter/paragraph. |
| `goto_next_function_collapsed` | Jump to the next function and collapse the selection to a cursor. |
| `goto_prev_function_collapsed` | Jump to the previous function and collapse the selection to a cursor. |

`goto_next_section` / `goto_prev_section` work on any of the following LaTeX
structural nodes: `\part`, `\chapter`, `\section`, `\subsection`,
`\subsubsection`, `\paragraph`, `\subparagraph`.

`goto_next_function_collapsed` / `goto_prev_function_collapsed` combine a
function jump with a selection collapse so the cursor lands on a single point
rather than spanning the whole block (useful for code or any language where
`@function.around` is defined).

## Default keybindings

The section bindings live in the `z` / `Z` (view) menu; the collapsed-function
bindings are in normal mode:

| Key | Command                         |
| --- | ------------------------------- |
| `zs` | `goto_next_section`            |
| `zS` | `goto_prev_section`            |
| `ü`  | `goto_next_function_collapsed` |
| `Ü`  | `goto_prev_function_collapsed` |

(`ü` / `Ü` are the umlaut keys on a German keyboard layout.)

Bind the static commands to your own keys in `config.toml` if you prefer, e.g.:

```toml
[keys.normal]
"]s" = "goto_next_section"
"[s" = "goto_prev_section"
```

## Tree-sitter captures

The LaTeX `textobjects.scm` now defines three captures:

| Capture           | Nodes matched                                                        |
| ----------------- | -------------------------------------------------------------------- |
| `@function.around` | `chapter`, `part`, `section`, `subsection`, `subsubsection`, `paragraph`, `subparagraph` |
| `@class.around`    | same set                                                            |
| `@section.around`  | same set (used exclusively by `goto_next/prev_section`)             |

`@function.around` was previously mapped to `generic_command`; it now maps to
structural sectioning nodes so that fold-by-function (`zM`) and jump-by-function
work sensibly in LaTeX documents.

## Behavior and limitations

- Navigation uses the same `goto_ts_object_impl` used by `goto_next_function` /
  `goto_next_class`, so it wraps around at buffer boundaries.
- The entire sectioning block (from the command to the start of the next sibling)
  is selected; use `goto_next_function_collapsed` / `ü` if you only want a cursor.
- Works in any `.tex` file with a tree-sitter grammar; no extra config required.
- Nested sections (a `\subsection` inside a `\section`) are each treated as
  independent nodes — the jump moves to the nearest next/previous node regardless
  of nesting depth.
