//! Compile the tree-sitter LaTeX grammar from C sources.
//!
//! We do NOT depend on the `tree-sitter-latex` crate: its published 0.1.0
//! `parser.c` references an external scanner but the package omits `scanner.c`,
//! so it fails to link. Instead we compile the grammar's `parser.c` +
//! `scanner.c` directly. By default we use the complete grammar already
//! vendored in this repo at `runtime/grammars/sources/latex/src`; the location
//! can be overridden with the `HELIX_SPELL_LATEX_SRC` environment variable.

use std::path::PathBuf;

fn main() {
    let src_dir = latex_src_dir();
    let parser = src_dir.join("parser.c");
    let scanner = src_dir.join("scanner.c");

    if !parser.is_file() {
        panic!(
            "LaTeX grammar source not found at {}.\n\
             This crate compiles the grammar vendored under the Helix repo's \
             runtime/grammars (which is .gitignored and populated by the grammar \
             fetch step). Run `hx --grammar fetch` (or `cargo run -- --grammar fetch` \
             from the repo root) to download it, or set HELIX_SPELL_LATEX_SRC to a \
             directory containing parser.c and scanner.c from \
             https://github.com/latex-lsp/tree-sitter-latex.",
            parser.display()
        );
    }

    let mut build = cc::Build::new();
    build.include(&src_dir).std("c11").warnings(false);
    build.file(&parser);
    println!("cargo:rerun-if-changed={}", parser.display());
    if scanner.is_file() {
        build.file(&scanner);
        println!("cargo:rerun-if-changed={}", scanner.display());
    }
    build.compile("tree-sitter-latex");

    println!("cargo:rerun-if-env-changed=HELIX_SPELL_LATEX_SRC");
}

fn latex_src_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("HELIX_SPELL_LATEX_SRC") {
        return PathBuf::from(dir);
    }
    // Default: this crate lives inside the Helix repo; the grammar is vendored
    // one level up under runtime/grammars/sources.
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    manifest.join("../runtime/grammars/sources/latex/src")
}
