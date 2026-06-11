//! Server configuration, parsed from the LSP `initializationOptions`, plus
//! the optional per-project config file (`.spell.toml` at the workspace root).
//!
//! Helix forwards the `config = { … }` table of a `[language-server.*]` entry
//! as `initializationOptions`. All fields are optional and have sensible
//! defaults so the server works out of the box with the system `en_US`
//! dictionary. Project-file fields override their global counterparts; see
//! [`effective_spelling`].

use std::fs;
use std::path::Path;

use serde::Deserialize;
use tower_lsp::lsp_types::DiagnosticSeverity;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct Config {
    /// Dictionary name to look up in the standard dictionary directories
    /// (e.g. `"en_US"` → `en_US.aff` + `en_US.dic`). Ignored if explicit
    /// `aff-path`/`dic-path` are given or if `dictionaries` is non-empty.
    pub dictionary: String,
    /// Dictionary names to load together (mixed mode: a word is correct if
    /// any of them knows it). Takes precedence over `dictionary`.
    pub dictionaries: Vec<String>,
    /// Restrict checking to this single dictionary (overridable per project
    /// via the project config file).
    pub language: Option<String>,
    /// Explicit path to the Hunspell `.aff` file (overrides `dictionary`).
    pub aff_path: Option<String>,
    /// Explicit path to the Hunspell `.dic` file (overrides `dictionary`).
    pub dic_path: Option<String>,
    /// File name (relative to the workspace root) of the project word list.
    pub project_dict_file: String,
    /// File name (relative to the workspace root) of the per-project config.
    pub project_config_file: String,
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
            dictionaries: Vec::new(),
            language: None,
            aff_path: None,
            dic_path: None,
            project_dict_file: ".spell.dic".to_string(),
            project_config_file: ".spell.toml".to_string(),
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

/// Per-project configuration (`.spell.toml` at the workspace root). Every
/// field set here overrides the corresponding `initializationOptions` field.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct ProjectConfig {
    /// Restrict checking to this single dictionary.
    pub language: Option<String>,
    /// Dictionary names to load together (mixed mode).
    pub dictionaries: Option<Vec<String>>,
}

impl ProjectConfig {
    /// Parse the project config file. `Ok(None)` if the file does not exist;
    /// `Err` (with a human-readable message) if it exists but is unreadable
    /// or malformed.
    pub fn load(path: &Path) -> Result<Option<Self>, String> {
        if !path.is_file() {
            return Ok(None);
        }
        let contents =
            fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        toml::from_str(&contents)
            .map(Some)
            .map_err(|e| format!("parsing {}: {e}", path.display()))
    }
}

/// The spelling setup after merging the project config over the global one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveSpelling {
    /// Base dictionary names, deduped, order-preserving. Empty when the user
    /// relies solely on explicit `aff-path`/`dic-path`.
    pub names: Vec<String>,
    /// `Some(lang)` restricts checking to that single dictionary.
    pub language: Option<String>,
}

/// Merge `project` over `config`, field by field. `Some(vec![])` and
/// empty/whitespace `language` values count as unset.
pub fn effective_spelling(config: &Config, project: &ProjectConfig) -> EffectiveSpelling {
    let nonempty = |s: &String| !s.trim().is_empty();
    let language = project
        .language
        .as_ref()
        .filter(|s| nonempty(s))
        .or(config.language.as_ref().filter(|s| nonempty(s)))
        .map(|s| s.trim().to_string());

    let names: Vec<String> = match project.dictionaries.as_ref().filter(|d| !d.is_empty()) {
        Some(dicts) => dicts.clone(),
        None if !config.dictionaries.is_empty() => config.dictionaries.clone(),
        // Explicit aff/dic paths replace the `dictionary` name (legacy
        // single-dictionary behavior), so no name to resolve.
        None if config.aff_path.is_some() && config.dic_path.is_some() => Vec::new(),
        None => vec![config.dictionary.clone()],
    };

    let mut seen = std::collections::HashSet::new();
    let names = names
        .into_iter()
        .filter(|n| seen.insert(n.clone()))
        .collect();

    EffectiveSpelling { names, language }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_project(toml_str: &str) -> ProjectConfig {
        toml::from_str(toml_str).expect("valid project config")
    }

    #[test]
    fn project_config_parses_full_partial_and_empty() {
        let full = parse_project("language = \"de_DE\"\ndictionaries = [\"en_US\", \"de_DE\"]");
        assert_eq!(full.language.as_deref(), Some("de_DE"));
        assert_eq!(
            full.dictionaries,
            Some(vec!["en_US".to_string(), "de_DE".to_string()])
        );

        let partial = parse_project("language = \"de_DE\"");
        assert_eq!(partial.language.as_deref(), Some("de_DE"));
        assert_eq!(partial.dictionaries, None);

        let empty = parse_project("");
        assert_eq!(empty.language, None);
        assert_eq!(empty.dictionaries, None);
    }

    #[test]
    fn project_config_rejects_malformed_toml() {
        assert!(toml::from_str::<ProjectConfig>("language = [").is_err());
    }

    #[test]
    fn old_initialization_options_still_parse_with_defaults() {
        let config = Config::from_value(Some(serde_json::json!({
            "dictionary": "en_GB",
            "severity": "warning",
        })));
        assert_eq!(config.dictionary, "en_GB");
        assert!(config.dictionaries.is_empty());
        assert_eq!(config.language, None);
        assert_eq!(config.project_config_file, ".spell.toml");
    }

    #[test]
    fn effective_defaults_to_single_dictionary_name() {
        let eff = effective_spelling(&Config::default(), &ProjectConfig::default());
        assert_eq!(eff.names, vec!["en_US".to_string()]);
        assert_eq!(eff.language, None);
    }

    #[test]
    fn global_dictionaries_win_over_dictionary() {
        let config = Config {
            dictionaries: vec!["en_US".into(), "de_DE".into()],
            ..Config::default()
        };
        let eff = effective_spelling(&config, &ProjectConfig::default());
        assert_eq!(eff.names, vec!["en_US".to_string(), "de_DE".to_string()]);
    }

    #[test]
    fn project_overrides_global_field_by_field() {
        let config = Config {
            dictionaries: vec!["en_US".into(), "de_DE".into()],
            language: Some("en_US".into()),
            ..Config::default()
        };
        let project = ProjectConfig {
            language: Some("de_DE".into()),
            dictionaries: Some(vec!["fr_FR".into()]),
        };
        let eff = effective_spelling(&config, &project);
        assert_eq!(eff.names, vec!["fr_FR".to_string()]);
        assert_eq!(eff.language.as_deref(), Some("de_DE"));
    }

    #[test]
    fn empty_project_values_count_as_unset() {
        let config = Config {
            dictionaries: vec!["en_US".into()],
            language: Some("en_US".into()),
            ..Config::default()
        };
        let project = ProjectConfig {
            language: Some("  ".into()),
            dictionaries: Some(vec![]),
        };
        let eff = effective_spelling(&config, &project);
        assert_eq!(eff.names, vec!["en_US".to_string()]);
        assert_eq!(eff.language.as_deref(), Some("en_US"));
    }

    #[test]
    fn explicit_paths_suppress_the_default_name() {
        let config = Config {
            aff_path: Some("/x/custom.aff".into()),
            dic_path: Some("/x/custom.dic".into()),
            ..Config::default()
        };
        let eff = effective_spelling(&config, &ProjectConfig::default());
        assert!(eff.names.is_empty());

        // ... but not an explicit `dictionaries` list.
        let config = Config {
            dictionaries: vec!["de_DE".into()],
            ..config
        };
        let eff = effective_spelling(&config, &ProjectConfig::default());
        assert_eq!(eff.names, vec!["de_DE".to_string()]);
    }

    #[test]
    fn names_are_deduped_preserving_order() {
        let config = Config {
            dictionaries: vec!["en_US".into(), "de_DE".into(), "en_US".into()],
            ..Config::default()
        };
        let eff = effective_spelling(&config, &ProjectConfig::default());
        assert_eq!(eff.names, vec!["en_US".to_string(), "de_DE".to_string()]);
    }
}
