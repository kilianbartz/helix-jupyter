# Jupyter REPL for Helix

Evaluate code from a buffer in a persistent [Jupyter](https://jupyter.org/)
kernel, see the output rendered inline below the lines you ran, and inspect the
values of the variables in your selection — similar to the REPL in editors like
Zed.

Because the kernel stays alive between evaluations, state is preserved: define a
variable in one selection and use it in the next without re-running anything.

---

## Table of contents

1. [How it works](#how-it-works)
2. [Prerequisites: installing a kernel](#prerequisites-installing-a-kernel)
3. [Configuration](#configuration)
4. [Commands](#commands)
5. [Typical workflow](#typical-workflow)
6. [Features in detail](#features-in-detail)
7. [Suggested keybindings](#suggested-keybindings)
8. [Theming](#theming)
9. [Limitations & deferred features](#limitations--deferred-features)
10. [Troubleshooting](#troubleshooting)
11. [For developers](#for-developers)

---

## How it works

The feature is implemented in the editor core (it is **not** a language server —
LSP has no concept of code execution). It is modeled on Helix's debugger (DAP)
integration:

- A new crate, `helix-jupyter`, spawns and talks to a kernel over the Jupyter
  ZeroMQ messaging protocol (using the `jupyter-zmq-client` and
  `jupyter-protocol` crates, which handle the wire protocol, HMAC signing and
  kernelspec discovery).
- The `Editor` owns a kernel registry; kernel messages flow back through the
  normal editor event loop and are rendered asynchronously.
- Each evaluation's output is anchored to the line you ran and survives edits
  (it is re-positioned as you insert/delete text, just like diagnostics).

A kernel is associated **per document**. Different buffers can run against
different kernels.

---

## Prerequisites: installing a kernel

You need a Jupyter kernel installed and discoverable, i.e. a *kernelspec* in one
of Jupyter's data directories (e.g. `~/.local/share/jupyter/kernels/<name>/`).
Helix discovers kernels the same way Jupyter does (including virtualenv kernels
reported by `jupyter --paths`).

### Option A — system Python with pip

```sh
pip install ipykernel
python -m ipykernel install --user --name python3 --display-name "Python 3"
```

### Option B — an isolated environment with `uv` (recommended)

This keeps the kernel and its packages isolated from your system Python:

```sh
uv venv ~/.venvs/helix-repl --python 3.12
uv pip install --python ~/.venvs/helix-repl/bin/python ipykernel numpy pandas
~/.venvs/helix-repl/bin/python -m ipykernel install --user \
    --name helix-repl --display-name "Helix REPL (py3.12)"
```

### Verify it is discoverable

```sh
jupyter kernelspec list      # if you have the `jupyter` CLI
# or just check the directory:
ls ~/.local/share/jupyter/kernels/
```

The directory name (e.g. `helix-repl`) is the **kernelspec name** you pass to
`:jupyter-start` or set as `default-kernel`.

Other languages work too, as long as a kernelspec is installed (e.g. `ir` for R,
`xeus-cling` for C++). Variable inspection (see below) is Python-specific.

---

## Configuration

Add a `[editor.jupyter]` section to your `config.toml`
(`~/.config/helix/config.toml`):

```toml
[editor.jupyter]
enable = true                 # master switch for the feature
default-kernel = "helix-repl" # kernelspec started by :jupyter-start with no arg
auto-start = true             # start the default kernel on first :jupyter-eval
inline-output = true          # render output below the evaluated lines
inline-images = true          # render plots/images inline on kitty-graphics terminals
max-output-lines = 20         # cap inline output per evaluation (excess summarized)
inspect-variables = true      # probe variable values for :jupyter-variables
```

| Option              | Type            | Default | Description                                                                 |
| ------------------- | --------------- | ------- | --------------------------------------------------------------------------- |
| `enable`            | bool            | `true`  | Turn the whole feature on/off.                                              |
| `default-kernel`    | string \| unset | unset   | Kernelspec name used by `:jupyter-start` (no arg) and by auto-start, unless an active venv kernel is detected (see below). |
| `auto-start`        | bool            | `true`  | If no kernel is running, `:jupyter-eval` auto-starts `default-kernel`.      |
| `inline-output`     | bool            | `true`  | Render stdout/stderr/results as virtual lines under the evaluated code.     |
| `inline-images`     | bool            | `true`  | Render image output (e.g. plots) as graphics on terminals supporting the kitty graphics protocol; text placeholder otherwise. |
| `max-output-lines`  | integer         | `20`    | Maximum inline lines per evaluation; extra lines are summarized.            |
| `inspect-variables` | bool            | `true`  | After each eval, probe the kernel for the values of variables you ran.      |

> If `default-kernel` is unset and `auto-start` can't pick one, `:jupyter-eval`
> will ask you to run `:jupyter-start <kernel>` (or `:jupyter-kernel-select`)
> first.

### Active virtualenv kernels

When you launch Helix from an activated Python virtual environment (so
`VIRTUAL_ENV` is set) and that venv has the `jupyter` CLI installed, Helix uses
the venv's own kernel as the default for `:jupyter-start` (no arg) and
auto-start. This takes precedence over `default-kernel`, so activating a venv
"just works" without per-project configuration.

The kernel is matched by interpreter: Helix picks the first installed kernelspec
whose interpreter lives inside `$VIRTUAL_ENV`. Installing `ipykernel` in the venv
(`pip install ipykernel`) is enough to make this kernel discoverable — its
built-in `python3` kernelspec under `$VIRTUAL_ENV/share/jupyter` points at the
venv's Python. Pass an explicit name to `:jupyter-start <kernel>` to override.

---

## Commands

All commands are available on the command line (`:`) and also as bindable
("static") commands for your keymap. Aliases are shown in parentheses.

| Command line                       | Static command          | What it does                                                                 |
| ---------------------------------- | ----------------------- | --------------------------------------------------------------------------- |
| `:jupyter-start [kernel]` (`jstart`) | —                       | Start a kernel for the current document. Uses `default-kernel` if no arg.    |
| `:jupyter-kernel-select` (`jkernel`) | `jupyter_kernel_select` | Open a picker of installed kernelspecs and start the chosen one.             |
| `:jupyter-eval` (`jeval`)          | `jupyter_eval`          | Evaluate the selected lines (or the current line) in the kernel.             |
| `:jupyter-variables` (`jvars`)     | `jupyter_variables`     | Show a popup of the variables touched by the last evaluation and their values. |
| `:jupyter-restart`                 | `jupyter_restart`       | Restart the kernel (clears all state and output) and start it again.         |
| `:jupyter-stop` (`jstop`)          | `jupyter_stop`          | Shut down the kernel and clear its output for this document.                 |

---

## Typical workflow

1. **Open a file** (e.g. a `.py` file).
2. **Start a kernel** — either:
   - `:jupyter-start helix-repl`, or
   - `:jupyter-kernel-select` to pick from a list, or
   - just run `:jupyter-eval` (auto-starts `default-kernel` if configured).
3. **Select the lines** you want to run and **`:jupyter-eval`**.
   The output appears as dimmed virtual lines directly below your selection.
4. **Select the next lines** and evaluate again — they run in the *same* kernel,
   so earlier definitions are still in scope.
5. **`:jupyter-variables`** to pop up the values of the variables from your last
   evaluation. Press **`Esc`** to close the popup.
6. **`:jupyter-restart`** to wipe state and start fresh, or **`:jupyter-stop`**
   when you're done.

---

## Features in detail

### Evaluating code

`:jupyter-eval` evaluates **whole lines**: every line that your selection
touches is sent to the kernel, even if the selection only partially covers them.
With no real selection (just a cursor), the current line is evaluated. This makes
"put the cursor on a line and run it" and "select a block and run it" both behave
the way you'd expect in a REPL.

State persists between evaluations for the lifetime of the kernel.

### Inline output

When `inline-output` is enabled, the result of an evaluation is rendered as
virtual lines beneath the last evaluated line:

```
    1  print("value is", 6 * 7)
       value is 42
```

- **stdout** and **results** (the value of the last expression) use the output
  style; **stderr** and **error tracebacks** use the error style.
- Output is capped at `max-output-lines`; beyond that you'll see a
  `… N more lines` summary.
- Tracebacks have their ANSI color codes stripped for clean terminal rendering.
- Re-evaluating the same line **replaces** its previous output block.
- Output stays attached to the correct line as you edit the buffer above it.

### Inline images (plots)

When `inline-images` is enabled and your terminal supports the **kitty graphics
protocol** (kitty, Ghostty, WezTerm, Konsole), image output — e.g. a matplotlib
figure — is rendered as an actual picture in the virtual lines below the code,
just like a notebook:

- Images are drawn using kitty's Unicode-placeholder placement, so they scroll
  and clip together with the surrounding text.
- The image is scaled to fit the view width while preserving its aspect ratio.
- Re-evaluating replaces the image, and restarting/stopping the kernel removes it.
- On terminals without graphics support (or with `inline-images = false`), a
  `[image WIDTH×HEIGHT]` text placeholder is shown instead.

> Only `image/png` output is rendered as graphics (matplotlib's inline default).
> Vector formats like `image/svg+xml` fall back to their text representation.

### Variable inspector

When `inspect-variables` is enabled, each evaluation triggers a second, silent
probe that asks the kernel for the current value of every identifier that
appeared in your selection. `:jupyter-variables` then shows them in a popup:

```
name = 'helix'
x = 42
```

- The inspector prefers the block on the current line; otherwise it shows the
  most recent evaluation that has variables.
- It reports non-callable, non-module globals (data variables), so functions,
  classes and imported modules are filtered out.
- **Close the popup with `Esc`** (or `Ctrl-c`). Scroll long lists with
  `Ctrl-d`/`Ctrl-u` or `PageDown`/`PageUp`.

> Variable inspection is implemented for Python kernels (it uses a small Python
> probe expression). Other languages will still evaluate and show inline output,
> but the inspector will be empty.

### Managing kernels

- `:jupyter-kernel-select` lists installed kernelspecs (name, display name,
  language) in a fuzzy picker and starts the one you choose for the current
  document.
- `:jupyter-restart` shuts the kernel down, clears all output and persisted
  state, and starts the same kernel again.
- `:jupyter-stop` shuts the kernel down and removes its output.

---

## Suggested keybindings

Bindable static commands let you wire the feature into your keymap. Example using
a `space j` ("Jupyter") submenu in `config.toml`:

```toml
[keys.normal.space.j]
e = "jupyter_eval"
v = "jupyter_variables"
k = "jupyter_kernel_select"
r = "jupyter_restart"
s = "jupyter_stop"
```

You can also bind to the command-line forms directly, e.g.
`e = ":jupyter-eval"`. A handy pattern is to select lines in visual mode and then
trigger eval.

---

## Theming

The inline output uses these theme scopes, with sensible fallbacks if your theme
doesn't define them:

| Scope                        | Used for                       | Fallback                                  |
| ---------------------------- | ------------------------------ | ----------------------------------------- |
| `ui.virtual.jupyter.output`  | stdout / result text           | `ui.virtual.inlay-hint` → `ui.virtual`    |
| `ui.virtual.jupyter.error`   | stderr / tracebacks            | `error`                                   |

Add them to your theme to customize, e.g.:

```toml
"ui.virtual.jupyter.output" = { fg = "gray", modifiers = ["italic"] }
"ui.virtual.jupyter.error"  = { fg = "red" }
```

---

## Limitations & deferred features

- **Interactive `input()`**: if kernel code calls `input()`, Helix currently
  replies with an empty string so the kernel doesn't hang. Interactive prompting
  is not yet implemented.
- **No dead-kernel detection**: if a kernel process dies unexpectedly, Helix
  won't notice automatically; use `:jupyter-restart`.
- **Rich output**: `image/png` is rendered as graphics on kitty-graphics
  terminals (see [Inline images](#inline-images-plots)); other rich types (HTML,
  SVG, …) are reduced to their `text/plain` representation. Inline images aren't
  re-scaled on terminal resize until the next evaluation.
- **Variable inspection is Python-specific.**

---

## Troubleshooting

**"No kernel selected" / eval does nothing**
Set `editor.jupyter.default-kernel`, or run `:jupyter-start <name>` /
`:jupyter-kernel-select` first.

**"No Jupyter kernelspecs found" in the picker**
No kernelspec is installed/discoverable. Install one (see
[Prerequisites](#prerequisites-installing-a-kernel)) and verify with
`ls ~/.local/share/jupyter/kernels/`.

**`Failed to start kernel '<name>'`**
The kernelspec name is wrong or its interpreter/`ipykernel` is missing. Check the
name matches a directory under a Jupyter kernels path, and that the environment
actually has `ipykernel` installed.

**No inline output appears**
Ensure `editor.jupyter.enable` and `editor.jupyter.inline-output` are `true`, and
that the code actually produces output (a bare assignment like `x = 1` prints
nothing — use `:jupyter-variables` to see its value).

**The variables popup is empty**
You need `inspect-variables = true`, a Python kernel, and at least one evaluation
that referenced/assigned variables.

---

## For developers

- Crate: `helix-jupyter/` (`Client`, `registry::Registry`, `Payload`).
- A standalone smoke tool that spawns a kernel and prints its output:
  ```sh
  cargo run -p helix-jupyter --example spike -- <kernelspec-name>
  ```
- Integration tests (require a kernelspec named `helix-test`; they skip
  themselves if it's absent):
  ```sh
  cargo test -p helix-term --features integration --test integration jupyter
  ```
- Editor integration lives in `helix-view` (`handlers/jupyter.rs`, `jupyter.rs`,
  the `Document`/`Editor` fields) and `helix-term` (`commands/jupyter.rs`,
  `ui/text_decorations/jupyter.rs`).
- Inline images use the kitty graphics protocol: PNGs are transmitted by the
  terminal backend (`helix-tui` `backend/{mod,termina}.rs`,
  `Backend::transmit_image`/`delete_image`), orchestrated from the render loop
  (`helix-term` `application.rs` `sync_jupyter_images`), and drawn as Unicode
  placeholder cells (`helix-term` `ui/text_decorations/kitty.rs`). Image
  ids/placements are tracked on `Editor` and `helix_view::jupyter::JupyterImage`.
