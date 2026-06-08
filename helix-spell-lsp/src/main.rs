//! `helix-spell-lsp` — a tree-sitter-aware spell-checking language server for
//! LaTeX, Markdown and Typst. Communicates over stdio. See `SPELL.md` for usage.

mod backend;
mod config;
mod dictionary;
mod extract;
mod position;

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
