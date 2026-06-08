//! Compile the tree-sitter LaTeX and Typst grammars from C sources.
//!
//! We do NOT depend on the published `tree-sitter-latex`/`tree-sitter-typst`
//! crates: the LaTeX 0.1.0 `parser.c` references an external scanner but the
//! package omits `scanner.c`, so it fails to link. Instead we compile each
//! grammar's `parser.c` + `scanner.c` directly. By default we use the complete
//! grammars already vendored in this repo at `runtime/grammars/sources/<name>/src`;
//! each location can be overridden with the `HELIX_SPELL_<NAME>_SRC` env var
//! (e.g. `HELIX_SPELL_LATEX_SRC`, `HELIX_SPELL_TYPST_SRC`).

use std::path::PathBuf;

/// A vendored tree-sitter grammar we compile: the subdirectory name under
/// `runtime/grammars/sources`, the env var overriding its `src` directory, and
/// the cc output library name.
struct Grammar {
    name: &'static str,
    src_env: &'static str,
    lib: &'static str,
}

const GRAMMARS: &[Grammar] = &[
    Grammar {
        name: "latex",
        src_env: "HELIX_SPELL_LATEX_SRC",
        lib: "tree-sitter-latex",
    },
    Grammar {
        name: "typst",
        src_env: "HELIX_SPELL_TYPST_SRC",
        lib: "tree-sitter-typst",
    },
];

fn main() {
    for grammar in GRAMMARS {
        compile(grammar);
        println!("cargo:rerun-if-env-changed={}", grammar.src_env);
    }
}

fn compile(grammar: &Grammar) {
    let src_dir = src_dir(grammar);
    let parser = src_dir.join("parser.c");
    let scanner = src_dir.join("scanner.c");

    if !parser.is_file() {
        panic!(
            "{} grammar source not found at {}.\n\
             This crate compiles the grammar vendored under the Helix repo's \
             runtime/grammars (which is .gitignored and populated by the grammar \
             fetch step). Run `hx --grammar fetch` (or `cargo run -- --grammar fetch` \
             from the repo root) to download it, or set {} to a directory \
             containing the grammar's parser.c and scanner.c.",
            grammar.name,
            parser.display(),
            grammar.src_env,
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
    build.compile(grammar.lib);
}

fn src_dir(grammar: &Grammar) -> PathBuf {
    if let Ok(dir) = std::env::var(grammar.src_env) {
        return PathBuf::from(dir);
    }
    // Default: this crate lives inside the Helix repo; the grammars are vendored
    // one level up under runtime/grammars/sources.
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    manifest.join(format!("../runtime/grammars/sources/{}/src", grammar.name))
}
