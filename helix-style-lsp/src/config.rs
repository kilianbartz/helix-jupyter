//! Server configuration, parsed from the LSP `initializationOptions`, plus the
//! optional per-project config file (`.style.toml` at the workspace root).
//!
//! Helix forwards the `config = { … }` table of a `[language-server.*]` entry as
//! `initializationOptions`. All fields are optional and have sensible defaults
//! that target an OpenAI-compatible endpoint. The API key is never read from the
//! config directly — `api-key-env` names an environment variable to read it from,
//! keeping secrets out of committed config.

use std::fs;
use std::path::Path;

use serde::Deserialize;
use tower_lsp::lsp_types::DiagnosticSeverity;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct Config {
    /// Base URL of an OpenAI-compatible API (the part before `/chat/completions`).
    /// Examples: `https://api.openai.com/v1`, `https://openrouter.ai/api/v1`,
    /// `http://localhost:11434/v1` (Ollama).
    pub endpoint: String,
    /// Name of the environment variable holding the API key. The key itself is
    /// never put in the config. Leave empty for keyless local endpoints (Ollama).
    pub api_key_env: String,
    /// Model identifier passed straight to the API (e.g. `gpt-4o-mini`,
    /// `llama3.1`, `anthropic/claude-3.5-sonnet`).
    pub model: String,
    /// Sampling temperature for the requests.
    pub temperature: f32,
    /// Upper bound on completion tokens requested from the model.
    pub max_output_tokens: u32,
    /// How to force JSON replies: `json_schema` (constrain to a schema — most
    /// reliable, needs a provider that supports OpenAI structured outputs, e.g.
    /// Ollama/vLLM/OpenAI), `json_object` (looser JSON mode), or `off` (rely on
    /// the prompt only). Small/thinking models need this to avoid prose leaking
    /// into the reply.
    pub json_mode: String,
    /// Guard against sending huge (and expensive) inputs. Text beyond this many
    /// characters is rejected with a message rather than silently truncated.
    pub max_input_chars: usize,
    /// Number of rephrasing alternatives to request for a selection.
    pub rephrase_options: usize,
    /// Maximum number of synonyms to request for a single selected word.
    pub synonym_options: usize,
    /// Writing-style profile that tunes the review rubric. One of `scientific`,
    /// `general`, `casual`; anything else falls back to `general`.
    pub style_profile: String,
    /// Optional extra instructions appended to the system prompt (e.g. a project
    /// glossary or house-style notes).
    pub extra_instructions: Option<String>,
    /// Diagnostic severity for style issues: "error" | "warning" | "info" | "hint".
    pub severity: String,
    /// File name (relative to the workspace root) of the per-project config.
    pub project_config_file: String,
    /// Request timeout in seconds for a single LLM call.
    pub timeout_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            endpoint: "https://api.openai.com/v1".to_string(),
            api_key_env: "OPENAI_API_KEY".to_string(),
            model: "gpt-4o-mini".to_string(),
            temperature: 0.2,
            max_output_tokens: 2048,
            max_input_chars: 12_000,
            json_mode: "json_schema".to_string(),
            rephrase_options: 3,
            synonym_options: 10,
            style_profile: "scientific".to_string(),
            extra_instructions: None,
            severity: "info".to_string(),
            project_config_file: ".style.toml".to_string(),
            timeout_secs: 60,
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

    /// Resolve the API key from the configured environment variable, if any.
    pub fn api_key(&self) -> Option<String> {
        let name = self.api_key_env.trim();
        if name.is_empty() {
            return None;
        }
        std::env::var(name).ok().filter(|v| !v.is_empty())
    }

    /// Apply any fields set in the per-project config over this one.
    pub fn merge_project(&mut self, project: &ProjectConfig) {
        if let Some(v) = &project.endpoint {
            self.endpoint = v.clone();
        }
        if let Some(v) = &project.model {
            self.model = v.clone();
        }
        if let Some(v) = &project.style_profile {
            self.style_profile = v.clone();
        }
        if let Some(v) = &project.extra_instructions {
            self.extra_instructions = Some(v.clone());
        }
    }
}

/// Per-project configuration (`.style.toml` at the workspace root). Every field
/// set here overrides the corresponding `initializationOptions` field. Secrets
/// (the API key env name) are intentionally *not* overridable per project.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct ProjectConfig {
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub style_profile: Option<String>,
    /// Project glossary / house-style notes appended to the system prompt.
    pub extra_instructions: Option<String>,
}

impl ProjectConfig {
    /// Parse the project config file. `Ok(None)` if the file does not exist;
    /// `Err` (with a human-readable message) if it exists but is malformed.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_target_openai() {
        let c = Config::default();
        assert_eq!(c.endpoint, "https://api.openai.com/v1");
        assert_eq!(c.api_key_env, "OPENAI_API_KEY");
        assert_eq!(c.style_profile, "scientific");
    }

    #[test]
    fn partial_init_options_keep_defaults() {
        let c = Config::from_value(Some(serde_json::json!({
            "endpoint": "http://localhost:11434/v1",
            "model": "llama3.1",
            "api-key-env": "",
        })));
        assert_eq!(c.endpoint, "http://localhost:11434/v1");
        assert_eq!(c.model, "llama3.1");
        assert_eq!(c.api_key_env, "");
        // untouched fields keep defaults
        assert_eq!(c.rephrase_options, 3);
        assert_eq!(c.json_mode, "json_schema");
        assert_eq!(c.severity, "info");
    }

    #[test]
    fn empty_api_key_env_yields_no_key() {
        let mut c = Config::default();
        c.api_key_env = "  ".to_string();
        assert_eq!(c.api_key(), None);
    }

    #[test]
    fn project_overrides_field_by_field() {
        let mut c = Config::default();
        let p = ProjectConfig {
            model: Some("gpt-4o".into()),
            extra_instructions: Some("Prefer British spelling.".into()),
            ..Default::default()
        };
        c.merge_project(&p);
        assert_eq!(c.model, "gpt-4o");
        assert_eq!(c.endpoint, "https://api.openai.com/v1"); // untouched
        assert_eq!(
            c.extra_instructions.as_deref(),
            Some("Prefer British spelling.")
        );
    }
}
