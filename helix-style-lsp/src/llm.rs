//! Minimal OpenAI-compatible chat-completions client.
//!
//! `POST {endpoint}/chat/completions` is understood by OpenAI, OpenRouter, Ollama
//! (`/v1`), vLLM, LM Studio and llama.cpp, so this one path covers every provider
//! the feature targets. Reliability comes from `response_format` (structured
//! outputs): per `Config::json_mode` we attach a JSON schema (or plain JSON mode)
//! so even small/thinking models return parseable JSON instead of leaking prose.
//! [`extract_json_object`] is still the final parse step as a belt-and-braces
//! fallback for providers that wrap the JSON.

use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::config::Config;
use crate::prompt;

/// One reported writing issue, anchored on a verbatim quote from the document.
#[derive(Debug, Clone, Deserialize)]
pub struct Issue {
    /// Exact substring of the document that has the problem.
    pub quote: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub explanation: String,
    /// Replacement for `quote`, or empty when there is no direct fix.
    #[serde(default)]
    pub suggestion: String,
}

#[derive(Debug, Deserialize)]
struct IssuesReply {
    /// Compact overall quality assessment shown in the summary popup.
    #[serde(default)]
    summary: String,
    #[serde(default)]
    issues: Vec<Issue>,
}

#[derive(Debug, Deserialize)]
struct AlternativesReply {
    #[serde(default)]
    alternatives: Vec<String>,
}

/// A configured LLM endpoint. Cheap to clone (wraps an `Arc` inside reqwest).
#[derive(Clone)]
pub struct Llm {
    http: reqwest::Client,
    endpoint: String,
    api_key: Option<String>,
    model: String,
    temperature: f32,
    max_output_tokens: u32,
    json_mode: JsonMode,
}

/// How replies are constrained to JSON.
#[derive(Clone, Copy, PartialEq, Eq)]
enum JsonMode {
    /// Attach a JSON schema (`response_format: json_schema`).
    Schema,
    /// Looser provider JSON mode (`response_format: json_object`).
    Object,
    /// No `response_format`; rely on the prompt alone.
    Off,
}

impl JsonMode {
    fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "false" => JsonMode::Off,
            "json_object" | "object" => JsonMode::Object,
            _ => JsonMode::Schema,
        }
    }
}

impl Llm {
    pub fn new(config: &Config) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs.max(1)))
            .build()
            .unwrap_or_default();
        Self {
            http,
            endpoint: config.endpoint.trim_end_matches('/').to_string(),
            api_key: config.api_key(),
            model: config.model.clone(),
            temperature: config.temperature,
            max_output_tokens: config.max_output_tokens,
            json_mode: JsonMode::parse(&config.json_mode),
        }
    }

    /// Review `text`; return a compact overall `summary` and the issues found.
    pub async fn check(
        &self,
        text: &str,
        profile: &str,
        extra: Option<&str>,
    ) -> Result<(String, Vec<Issue>), String> {
        let system = prompt::check_system(profile, extra);
        let content = self
            .complete(&system, text, Some(("writing_review", check_schema())))
            .await?;
        let obj = extract_json_object(&content)
            .ok_or_else(|| "model did not return a JSON object".to_string())?;
        let reply: IssuesReply =
            serde_json::from_str(&obj).map_err(|e| format!("parsing issues JSON: {e}"))?;
        let issues = reply
            .issues
            .into_iter()
            .filter(|i| !i.quote.is_empty())
            .collect();
        Ok((reply.summary, issues))
    }

    /// Produce exactly `n` rephrasings of `text` (the schema pins the count).
    pub async fn rephrase(
        &self,
        text: &str,
        n: usize,
        profile: &str,
        extra: Option<&str>,
    ) -> Result<Vec<String>, String> {
        let system = prompt::rephrase_system(n, profile, extra);
        let content = self
            .complete(&system, text, Some(("rephrasings", rephrase_schema(n))))
            .await?;
        let obj = extract_json_object(&content)
            .ok_or_else(|| "model did not return a JSON object".to_string())?;
        let reply: AlternativesReply =
            serde_json::from_str(&obj).map_err(|e| format!("parsing alternatives JSON: {e}"))?;
        let alts: Vec<String> = reply
            .alternatives
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if alts.is_empty() {
            return Err("model returned no alternatives".to_string());
        }
        Ok(alts)
    }

    /// One chat-completions round-trip; returns the assistant message content.
    /// `schema` is `(name, json_schema)` used when `json_mode` is `Schema`.
    async fn complete(
        &self,
        system: &str,
        user: &str,
        schema: Option<(&str, serde_json::Value)>,
    ) -> Result<String, String> {
        let url = format!("{}/chat/completions", self.endpoint);
        let mut body = json!({
            "model": self.model,
            "temperature": self.temperature,
            "max_tokens": self.max_output_tokens,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user },
            ],
        });

        if let Some(response_format) = self.response_format(schema) {
            body["response_format"] = response_format;
        }

        let mut req = self.http.post(&url).json(&body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| format!("request to {url} failed: {e}"))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| format!("reading response body: {e}"))?;
        if !status.is_success() {
            return Err(format!("API returned {status}: {}", truncate(&text, 300)));
        }

        let parsed: ChatResponse =
            serde_json::from_str(&text).map_err(|e| format!("parsing API response: {e}"))?;
        parsed
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .ok_or_else(|| "API response contained no choices".to_string())
    }

    /// Build the `response_format` value for the configured JSON mode, if any.
    fn response_format(
        &self,
        schema: Option<(&str, serde_json::Value)>,
    ) -> Option<serde_json::Value> {
        match self.json_mode {
            JsonMode::Off => None,
            JsonMode::Object => Some(json!({ "type": "json_object" })),
            JsonMode::Schema => match schema {
                Some((name, schema)) => Some(json!({
                    "type": "json_schema",
                    "json_schema": { "name": name, "schema": schema },
                })),
                None => Some(json!({ "type": "json_object" })),
            },
        }
    }
}

/// JSON schema for the writing review: a `summary` plus an `issues` array.
fn check_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string" },
            "issues": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "quote": { "type": "string" },
                        "category": {
                            "type": "string",
                            "enum": ["grammar", "style", "conciseness", "clarity"]
                        },
                        "explanation": { "type": "string" },
                        "suggestion": { "type": "string" }
                    },
                    "required": ["quote", "category", "explanation", "suggestion"]
                }
            }
        },
        "required": ["summary", "issues"]
    })
}

/// JSON schema for rephrasing: exactly `n` alternative strings.
fn rephrase_schema(n: usize) -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "alternatives": {
                "type": "array",
                "items": { "type": "string" },
                "minItems": n,
                "maxItems": n
            }
        },
        "required": ["alternatives"]
    })
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ChatMessage,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    #[serde(default)]
    content: String,
}

/// Pull the first balanced top-level `{ … }` object out of arbitrary model text.
/// Handles fenced ```json blocks and leading/trailing prose. String contents
/// (including escaped quotes and braces) are skipped so braces inside strings do
/// not unbalance the scan.
fn extract_json_object(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let start = s.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for i in start..bytes.len() {
        let c = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_string = false;
            }
            continue;
        }
        match c {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(s[start..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_bare_object() {
        let s = r#"{"issues": []}"#;
        assert_eq!(extract_json_object(s).as_deref(), Some(r#"{"issues": []}"#));
    }

    #[test]
    fn extracts_from_fenced_block_with_prose() {
        let s = "Sure!\n```json\n{\"alternatives\": [\"a\", \"b\"]}\n```\nHope that helps.";
        let obj = extract_json_object(s).unwrap();
        let reply: AlternativesReply = serde_json::from_str(&obj).unwrap();
        assert_eq!(reply.alternatives, vec!["a", "b"]);
    }

    #[test]
    fn braces_inside_strings_do_not_unbalance() {
        let s = r#"{"issues": [{"quote": "f(x) = {a}", "category": "style"}]}"#;
        let obj = extract_json_object(s).unwrap();
        let reply: IssuesReply = serde_json::from_str(&obj).unwrap();
        assert_eq!(reply.issues.len(), 1);
        assert_eq!(reply.issues[0].quote, "f(x) = {a}");
    }

    #[test]
    fn returns_none_when_no_object() {
        assert_eq!(extract_json_object("no json here"), None);
    }

    #[test]
    fn parses_summary_and_issues() {
        let s = r#"{"summary": "Concise but wordy in places.",
                    "issues": [{"quote": "in order to", "category": "conciseness",
                                "explanation": "wordy", "suggestion": "to"}]}"#;
        let reply: IssuesReply = serde_json::from_str(s).unwrap();
        assert_eq!(reply.summary, "Concise but wordy in places.");
        assert_eq!(reply.issues.len(), 1);
        assert_eq!(reply.issues[0].suggestion, "to");
    }

    #[test]
    fn json_mode_parses_aliases() {
        assert!(matches!(JsonMode::parse("json_schema"), JsonMode::Schema));
        assert!(matches!(JsonMode::parse("JSON_OBJECT"), JsonMode::Object));
        assert!(matches!(JsonMode::parse("off"), JsonMode::Off));
        assert!(matches!(JsonMode::parse("anything else"), JsonMode::Schema));
    }

    fn llm_with(mode: JsonMode) -> Llm {
        Llm {
            http: reqwest::Client::new(),
            endpoint: "http://x/v1".to_string(),
            api_key: None,
            model: "m".to_string(),
            temperature: 0.0,
            max_output_tokens: 1,
            json_mode: mode,
        }
    }

    #[test]
    fn response_format_per_mode() {
        let schema = || Some(("name", json!({"type": "object"})));
        assert_eq!(llm_with(JsonMode::Off).response_format(schema()), None);
        assert_eq!(
            llm_with(JsonMode::Object).response_format(schema()),
            Some(json!({ "type": "json_object" }))
        );
        let rf = llm_with(JsonMode::Schema)
            .response_format(schema())
            .unwrap();
        assert_eq!(rf["type"], "json_schema");
        assert_eq!(rf["json_schema"]["name"], "name");
        // Schema mode with no schema falls back to plain json_object.
        assert_eq!(
            llm_with(JsonMode::Schema).response_format(None),
            Some(json!({ "type": "json_object" }))
        );
    }

    #[test]
    fn rephrase_schema_pins_count() {
        let schema = rephrase_schema(3);
        let items = &schema["properties"]["alternatives"];
        assert_eq!(items["minItems"], 3);
        assert_eq!(items["maxItems"], 3);
    }
}
