//! `helix-style-lsp` — an LLM-backed grammar/writing-style language server.
//!
//! It adds two *manual* code actions to any registered document: "Check writing"
//! (grammar/style/conciseness diagnostics over the selection or whole file) and
//! "Rephrase selection" (pick one of several LLM rewrites). All analysis goes to
//! an OpenAI-compatible endpoint configured in `languages.toml`. Communicates
//! over stdio. See `STYLE.md` for usage.

mod backend;
mod config;
mod llm;
mod position;
mod prompt;

use backend::Backend;
use tower_lsp::{LspService, Server};

#[tokio::main]
async fn main() {
    // Logs go to stderr (LSP uses stdout for the protocol). Control verbosity
    // with RUST_LOG, e.g. `RUST_LOG=debug`.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
