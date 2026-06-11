# Writing style & grammar (LLM)

This fork ships **`helix-style-lsp`**, a language server that checks your prose
for **grammar, clarity and conciseness** with an LLM, and rewrites a selection
into a few **rephrasing** options. It targets scientific/academic writing by
default but works for any prose.

Unlike the [spell checker](SPELL.md), which runs continuously, the style checker
is **manual**: nothing is ever sent to the LLM until you ask for it from the
code-action menu (`<space>a`). It talks to any **OpenAI-compatible** endpoint, so
the same binary works with OpenAI, OpenRouter, a local Ollama, vLLM, LM Studio,
llama.cpp, and similar.

It is registered for **LaTeX, Markdown and Typst** alongside `texlab`/`marksman`/
`tinymist` and the spell checker, with `only-features = ["diagnostics",
"code-action"]`, so it never competes for completion, formatting, or navigation.

## What it does

Open the code-action menu with `<space>a`:

- **Check writing in document / selection (grammar & style)** — sends the text to
  the LLM and:
  - pops a **popup with a very compact overall evaluation** of the passage's
    quality (plus how many issues were highlighted), and
  - reports each issue as a **diagnostic** anchored on the offending phrase —
    highlighted exactly like a spelling mistake — tagged by category (`grammar`,
    `style`, `conciseness`, `clarity`) with a one-line explanation.

  With **no selection** the whole file is checked; with a selection only that
  range is checked.
  - Where the model proposes a concrete fix, a follow-up **`Apply: …`** quick-fix
    appears on that diagnostic (place the cursor on it and press `<space>a`).
- **Rephrase selection…** (only with a selection) — asks the LLM for **three**
  rewrites (configurable), then pops a **picker** that lists each full rewrite
  (numbered, wrapped so you can read the whole sentence) above the selectable
  items; choose one and it replaces the selection.

Markup is preserved: the prompt instructs the model to keep LaTeX/Typst commands,
math, and code (`\section{…}`, `\cite{…}`, `$…$`, …) verbatim in both rewrites and
fix suggestions. (Best-effort — very small models may still drop a command.)

Diagnostics are browsable like any other (`<space>d`), and shown inline / on
hover. Re-running a check **replaces** this server's previous diagnostics for the
document; spell-check and LSP diagnostics are unaffected.

## Prerequisites

- **The `helix-style-lsp` binary on `PATH`** (see [Building](#building)).
- **An OpenAI-compatible endpoint and (usually) an API key.** For a cloud
  provider, export your key into an environment variable and point `api-key-env`
  at it (default `OPENAI_API_KEY`). For a local Ollama no key is needed — set
  `api-key-env = ""`.

## Building

```sh
# from the repo root
cargo install --path helix-style-lsp --locked    # installs helix-style-lsp into ~/.cargo/bin
```

Or, for development, build it and point `languages.toml` `command` at the binary
by absolute path:

```sh
cargo build --release --manifest-path helix-style-lsp/Cargo.toml
# binary at helix-style-lsp/target/release/helix-style-lsp
```

## Configuration

Options are passed through the `config` table of the server entry in
`languages.toml` (forwarded to the server as `initializationOptions`). All are
optional; defaults target OpenAI with a scientific-writing profile.

```toml
[language-server.helix-style]
command = "helix-style-lsp"
config = { model = "gpt-4o-mini", style-profile = "scientific", api-key-env = "OPENAI_API_KEY" }
```

| Key                  | Default                       | Meaning                                                                 |
| -------------------- | ----------------------------- | ----------------------------------------------------------------------- |
| `endpoint`           | `https://api.openai.com/v1`   | Base URL of the API (the part before `/chat/completions`).              |
| `api-key-env`        | `"OPENAI_API_KEY"`            | **Name of an env var** holding the API key. The key is never put in the config. Empty = send no auth (local Ollama). |
| `model`              | `"gpt-4o-mini"`               | Model id passed to the API.                                             |
| `style-profile`      | `"scientific"`                | Review rubric: `scientific` \| `general` \| `casual`.                  |
| `extra-instructions` | —                             | Extra system-prompt text (project glossary / house style).             |
| `rephrase-options`   | `3`                           | How many rewrites to request for a selection (pinned exactly in `json_schema` mode). |
| `json-mode`          | `"json_schema"`               | How replies are forced to JSON: `json_schema` (constrain to a schema — most reliable) \| `json_object` (looser JSON mode) \| `off` (prompt only). See note below. |
| `severity`           | `"info"`                      | Diagnostic severity: `error` \| `warning` \| `info` \| `hint`.         |
| `max-input-chars`    | `12000`                       | Reject (don't truncate) inputs larger than this — a cost/latency guard. |
| `temperature`        | `0.2`                         | Sampling temperature.                                                   |
| `max-output-tokens`  | `2048`                        | Upper bound on completion length (thinking models spend some of this on reasoning). |
| `timeout-secs`       | `60`                          | Per-request timeout.                                                    |
| `project-config-file`| `".style.toml"`               | Per-project config file name, relative to the workspace root.          |

### Provider examples

```toml
# OpenRouter
config = { endpoint = "https://openrouter.ai/api/v1", api-key-env = "OPENROUTER_API_KEY", model = "anthropic/claude-3.5-sonnet" }

# Local Ollama (no key)
config = { endpoint = "http://localhost:11434/v1", api-key-env = "", model = "llama3.1" }
```

### Per-project overrides

A **`.style.toml`** at the workspace root overrides the global config field by
field (read once at startup; run `:lsp-restart` after editing it). Secrets are
not overridable here.

```toml
# .style.toml
model = "gpt-4o"
style-profile = "scientific"
extra-instructions = "Use British spelling. 'foo' and 'bar' are product names, not typos."
```

## Limitations

- **Costs tokens and takes seconds.** Every check/rephrase is a live API call; the
  editor shows a brief "checking…/rephrasing…" message while it runs. Keep
  selections tight and mind `max-input-chars`.
- **Diagnostics are anchored on the model's quoted phrase.** If the model quotes a
  span that does not appear verbatim in the text, that issue is dropped (and
  counted in the completion message) rather than mis-placed.
- **`json-mode` reliability vs. compatibility.** `json_schema` (the default)
  constrains the model to valid JSON and is what makes small/local models usable
  (without it they leak reasoning/markdown and you get "did not return JSON"
  errors). It needs an endpoint that supports OpenAI **structured outputs**
  (Ollama, vLLM, OpenAI, LM Studio do). If your provider rejects it, set
  `json-mode = "json_object"` or `"off"`.
- **LLM output is advisory and non-deterministic** — suggestions vary run to run
  and may be wrong. Nothing is changed without your confirmation.
- **No streaming / no continuous checking** — analysis runs only when you invoke
  it from `<space>a`.
- Markup (LaTeX commands, code, math) is sent as-is; the prompt asks the model to
  ignore it, but it is not stripped by tree-sitter the way the spell checker does.
