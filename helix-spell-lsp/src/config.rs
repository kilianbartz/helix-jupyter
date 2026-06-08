//! Server configuration, parsed from the LSP `initializationOptions`.
//!
//! Helix forwards the `config = { … }` table of a `[language-server.*]` entry
//! as `initializationOptions`. All fields are optional and have sensible
//! defaults so the server works out of the box with the system `en_US`
//! dictionary.

use serde::Deserialize;
use tower_lsp::lsp_types::DiagnosticSeverity;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct Config {
    /// Dictionary name to look up in the standard dictionary directories
    /// (e.g. `"en_US"` → `en_US.aff` + `en_US.dic`). Ignored if explicit
    /// `aff-path`/`dic-path` are given.
    pub dictionary: String,
    /// Explicit path to the Hunspell `.aff` file (overrides `dictionary`).
    pub aff_path: Option<String>,
    /// Explicit path to the Hunspell `.dic` file (overrides `dictionary`).
    pub dic_path: Option<String>,
    /// File name (relative to the workspace root) of the project word list.
    pub project_dict_file: String,
    /// Diagnostic severity for unknown words: "error" | "warning" | "info" | "hint".
    pub severity: String,
    /// Skip all-uppercase tokens (acronyms like `NASA`, `HTTP`).
    pub ignore_uppercase: bool,
    /// Maximum number of replacement suggestions offered per misspelling.
    pub max_suggestions: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            dictionary: "en_US".to_string(),
            aff_path: None,
            dic_path: None,
            project_dict_file: ".spell.dic".to_string(),
            severity: "info".to_string(),
            ignore_uppercase: true,
            max_suggestions: 5,
        }
    }
}

impl Config {
    /// Parse from `initializationOptions`; falls back to defaults on any error.
    pub fn from_value(value: Option<serde_json::Value>) -> Self {
        match value {
            Some(v) => serde_json::from_value(v).unwrap_or_default(),
            None => Config::default(),
        }
    }

    pub fn diagnostic_severity(&self) -> DiagnosticSeverity {
        match self.severity.to_ascii_lowercase().as_str() {
            "error" => DiagnosticSeverity::ERROR,
            "warning" | "warn" => DiagnosticSeverity::WARNING,
            "hint" => DiagnosticSeverity::HINT,
            _ => DiagnosticSeverity::INFORMATION,
        }
    }
}
