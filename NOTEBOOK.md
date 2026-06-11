# Jupyter Notebooks (`.ipynb`) in Helix

Open a Jupyter notebook like any other file: the JSON is converted to an
editable **percent-format** text buffer (cells separated by `# %%` markers),
the outputs saved in the file are rendered inline below their cells, and `:w`
writes valid nbformat JSON back — preserving notebook metadata, cell ids and
the outputs of cells you didn't re-run.

This builds on the [Jupyter REPL](REPL.md): the same kernels, inline output
rendering, inline images and variable inspector all work in notebook buffers.

---

## Table of contents

1. [How it works](#how-it-works)
2. [The percent format](#the-percent-format)
3. [Cells in plain files](#cells-in-plain-files)
4. [Commands and keybindings](#commands-and-keybindings)
5. [Saved outputs](#saved-outputs)
6. [Saving: what round-trips](#saving-what-round-trips)
7. [Visuals and theming](#visuals-and-theming)
8. [Language servers and linters](#language-servers-and-linters)
9. [Limitations](#limitations)

---

## How it works

When you open a file whose name ends in `.ipynb` and whose content parses as
an nbformat notebook:

- The buffer shows a **jupytext-style percent-format conversion** of the
  notebook: each cell becomes a `# %%` delimiter line followed by the cell's
  source. Markdown and raw cells are shown `# `-commented.
- The parsed notebook JSON is **retained in memory**. On `:w`, the buffer's
  cell sources are patched back into it and the JSON is written — everything
  the editor doesn't model (notebook metadata, cell ids, cell metadata,
  attachments, execution counts, outputs) round-trips untouched.
- The buffer is treated as the notebook's **source language** (from
  `metadata.kernelspec.language`, default Python), so highlighting and
  language servers work as in a `.py` file.
- The **diff gutter is disabled** for notebook documents: the buffer is a
  conversion, so a git diff against the on-disk JSON would mark every line as
  changed.
- `:reload` re-parses the file and re-converts (it refuses to reload a file
  that is no longer valid notebook JSON rather than clobbering your buffer).

A file ending in `.ipynb` that does *not* parse as a notebook opens as plain
JSON, unchanged.

Saving a non-notebook buffer to an `.ipynb` path (`:w new.ipynb`) creates a
fresh nbformat 4.5 notebook from the buffer's cells. Renaming a notebook
document to a non-`.ipynb` path turns it into a plain text buffer (the
percent-format text is what gets saved).

## The percent format

```python
# %% [markdown]
# # My analysis
#
# Some prose.

# %%
import numpy as np
x = np.arange(10)

# %% [raw]
# raw cell content
```

- A delimiter is a line starting with `# %%` (or `#%%`), optionally followed
  by `[markdown]` / `[md]` / `[raw]` and a free-form title.
- Markdown/raw cell content is commented: `# ` before each line, a bare `#`
  for empty lines. The prefix is stripped again on save.
- Text above the first delimiter is an *implicit* leading code cell.
- Cells are created, deleted, split and reordered **by editing the text** —
  add a `# %%` line to split a cell, delete one to merge two cells.

## Cells in plain files

Cell scanning is purely textual, so everything below — eval-cell, navigation,
the cell gutter, delimiter styling — also works in a plain `.py` (or any)
buffer that uses `# %%` markers. Only the JSON round-trip is specific to
`.ipynb` files.

## Commands and keybindings

| Command line                       | Static command      | Keys | What it does                                                       |
| ---------------------------------- | ------------------- | ---- | ------------------------------------------------------------------ |
| `:jupyter-eval-cell` (`jcell`)     | `jupyter_eval_cell` | —    | Evaluate the cell under the cursor (markdown/raw cells are skipped). |
| —                                  | `goto_next_cell`    | `]j` | Move to the first line of the next cell.                            |
| —                                  | `goto_prev_cell`    | `[j` | Move to the first line of the previous cell.                        |

All the [REPL commands](REPL.md#commands) (`:jupyter-start`, `:jupyter-eval`,
`:jupyter-variables`, …) work in notebook buffers too. A handy keymap:

```toml
[keys.normal.space.j]
c = "jupyter_eval_cell"
e = "jupyter_eval"
v = "jupyter_variables"
```

Evaluating a cell sends its body (without the delimiter line) to the
document's kernel and anchors the output below the cell's last line —
**replacing** the cell's previous output, including the one loaded from the
file.

## Saved outputs

On open, each code cell's stored `outputs` are rendered as inline virtual
lines below the cell, exactly like fresh evaluation output:

- `stream` text renders in the output style (stderr in the error style),
- `execute_result` / `display_data` render their `text/plain` — unless they
  carry an `image/png`, which renders as an inline image on kitty-graphics
  terminals (see [REPL.md → Inline images](REPL.md#inline-images-plots)),
- `error` tracebacks render in the error style, ANSI codes stripped.

## Saving: what round-trips

On `:w`, buffer cells are matched against the retained notebook's cells:

1. cells whose source is unchanged keep everything (including their exact
   `source` representation),
2. edited cells are paired with their original by position between unchanged
   neighbors and keep their id, metadata, execution count and outputs,
3. brand-new cells get a fresh id, `execution_count: null` and no outputs,
4. cells deleted from the buffer are dropped from the file.

Outputs follow these rules:

- A cell **re-executed in the editor** (via `:jupyter-eval-cell` /
  `:jupyter-eval`) gets its current inline output written back as nbformat
  outputs (stdout/stderr → `stream`, results → `execute_result`, images →
  `display_data`, tracebacks → `error`). Its `execution_count` becomes `null`.
- Every other cell keeps the outputs already in the file. Stopping or
  restarting the kernel clears *inline* output but never the file's outputs.

The file is written in nbformat's own style (1-space indent, trailing
newline). JSON key order may differ from Python's `nbformat` library, so the
first save can produce a larger git diff; content is preserved exactly.

## Visuals and theming

- The **cells gutter** (`cells` in `editor.gutters.layout`, on by default)
  draws a `▍` bar over each cell's extent, colored by cell type. It takes no
  space in buffers without `# %%` markers.
- **Delimiter lines** are styled via theme scopes (themes that don't define
  them just show the plain comment).

| Scope                                | Used for                                  |
| ------------------------------------ | ----------------------------------------- |
| `ui.virtual.jupyter.cell.code`       | `# %%` delimiter lines of code cells      |
| `ui.virtual.jupyter.cell.markdown`   | `# %% [markdown]`/`[raw]` delimiter lines |
| `ui.gutter.jupyter.cell.code`        | gutter bar of code cells (falls back to the scope above, then blue) |
| `ui.gutter.jupyter.cell.markdown`    | gutter bar of markdown/raw cells (fallback: the scope above, then yellow) |

```toml
"ui.virtual.jupyter.cell.code" = { fg = "blue", modifiers = ["bold"] }
"ui.virtual.jupyter.cell.markdown" = { fg = "yellow", modifiers = ["bold"] }
```

Output blocks use the existing `ui.virtual.jupyter.output` / `.error` scopes
(see [REPL.md → Theming](REPL.md#theming)).

## Language servers and linters

Language servers see the percent-format buffer as **one Python file** (under
the document's real `.ipynb` URI). Completion, hover, goto-definition and
type checking work normally, but lint rules that reason about *file* layout
can misfire across cell boundaries. The classic case is `E402` ("module level
import not at top of file/cell"): an import at the top of a *later* cell is
perfectly normal notebook style, but a linter that sees one concatenated file
flags it.

Because the URI keeps its `.ipynb` extension, such rules can be ignored for
notebooks only. For **ruff**, either in your project's `ruff.toml` /
`pyproject.toml`:

```toml
[lint.per-file-ignores]          # [tool.ruff.lint.per-file-ignores] in pyproject.toml
"*.ipynb" = ["E402"]
```

or globally in your Helix `languages.toml`:

```toml
[language-server.ruff.config.settings.lint.per-file-ignores]
"*.ipynb" = ["E402"]
```

## Limitations

- Lint rules with file-scope semantics (e.g. `E402`) can fire across cell
  boundaries — see [Language servers and linters](#language-servers-and-linters).
- A line starting with `# %%` **inside** a code cell's source is
  indistinguishable from a delimiter and will split the cell on save
  (jupytext has the same limitation).
- The comment token is hard-coded to `#`, so markdown cells of notebooks in
  `//`-comment languages won't decode their prefix on save.
- Cell titles after `# %%` are kept in the buffer but not stored in cell
  metadata.
- Rich output types other than `text/plain` and `image/png` are not rendered
  (they are preserved in the file).
- Cell ids are regenerated for split/new cells; `execution_count` is not
  tracked and becomes `null` on re-executed cells.
- Byte-identical output with Python's `nbformat` writer is not guaranteed
  (JSON key order); semantic content is.
