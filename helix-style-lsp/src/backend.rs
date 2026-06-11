//! The tower-lsp server: offers manual "check writing" and "rephrase" code
//! actions, runs them against an OpenAI-compatible LLM, surfaces results as
//! diagnostics (with quick-fix edits) and a rephrase picker.
//!
//! Modeled on `helix-spell-lsp`, but every analysis is *manual*: nothing is sent
//! to the LLM on open/change/save. The user triggers a check or a rephrase from
//! the code-action menu (`<space>a`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::json;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use crate::config::{Config, ProjectConfig};
use crate::llm::{Issue, Llm};
use crate::position::LineIndex;

const SOURCE: &str = "helix-style";
const CHECK_COMMAND: &str = "helix-style.check";
const REPHRASE_COMMAND: &str = "helix-style.rephrase";

#[derive(Default)]
struct State {
    config: Config,
    workspace_root: Option<PathBuf>,
    /// FULL-sync document text, keyed by URI.
    documents: HashMap<Url, String>,
    /// Composed once in `initialize` for the "ready" log line.
    status: String,
}

pub struct Backend {
    client: Client,
    state: Mutex<State>,
    /// Built from the effective config in `initialize`. `None` until then.
    llm: Mutex<Option<Llm>>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            state: Mutex::new(State::default()),
            llm: Mutex::new(None),
        }
    }

    /// Snapshot the data a command needs without holding the lock across `.await`.
    fn snapshot(&self) -> Option<(Llm, Config)> {
        let llm = self.llm.lock().unwrap().clone()?;
        let config = self.state.lock().unwrap().config.clone();
        Some((llm, config))
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(
        &self,
        params: InitializeParams,
    ) -> tower_lsp::jsonrpc::Result<InitializeResult> {
        let workspace_root = workspace_root(&params);
        let mut config = Config::from_value(params.initialization_options);

        let mut warnings: Vec<String> = Vec::new();

        // Per-project config (`.style.toml`) overrides the global one.
        if let Some(root) = &workspace_root {
            let path = root.join(&config.project_config_file);
            match ProjectConfig::load(&path) {
                Ok(Some(pc)) => config.merge_project(&pc),
                Ok(None) => {}
                Err(e) => warnings.push(format!("ignoring project config: {e}")),
            }
        }

        let has_key = config.api_key().is_some();
        if !has_key && config.api_key_env.trim().is_empty() {
            // Keyless endpoint (e.g. Ollama) — fine, just note it.
            warnings.push("no api-key-env set; sending requests without auth".to_string());
        } else if !has_key {
            warnings.push(format!(
                "env var `{}` is unset or empty; requests will be unauthenticated",
                config.api_key_env
            ));
        }

        let status = format!(
            "endpoint: {}, model: {}, profile: {}{}",
            config.endpoint,
            config.model,
            config.style_profile,
            if has_key { ", authenticated" } else { "" }
        );

        let llm = Llm::new(&config);

        for warning in &warnings {
            self.client
                .log_message(MessageType::WARNING, format!("helix-style: {warning}"))
                .await;
        }

        {
            let mut state = self.state.lock().unwrap();
            state.config = config;
            state.workspace_root = workspace_root;
            state.status = status;
        }
        *self.llm.lock().unwrap() = Some(llm);

        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "helix-style-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec![CHECK_COMMAND.to_string(), REPHRASE_COMMAND.to_string()],
                    ..Default::default()
                }),
                ..Default::default()
            },
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        let status = self.state.lock().unwrap().status.clone();
        self.client
            .log_message(
                MessageType::INFO,
                format!("helix-style-lsp ready ({status})"),
            )
            .await;
    }

    async fn shutdown(&self) -> tower_lsp::jsonrpc::Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let doc = params.text_document;
        self.state
            .lock()
            .unwrap()
            .documents
            .insert(doc.uri, doc.text);
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        // FULL sync: the final change carries the entire document text.
        if let Some(change) = params.content_changes.into_iter().last() {
            let mut state = self.state.lock().unwrap();
            if let Some(text) = state.documents.get_mut(&uri) {
                *text = change.text;
            } else {
                state.documents.insert(uri, change.text);
            }
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.state.lock().unwrap().documents.remove(&uri);
        // Clear any style diagnostics for the closed document.
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
    }

    async fn code_action(
        &self,
        params: CodeActionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<CodeActionResponse>> {
        let uri = params.text_document.uri;
        let range = params.range;
        let has_selection = range.start != range.end;

        let mut actions: Vec<CodeActionOrCommand> = Vec::new();

        // Quick-fixes for our own diagnostics under the cursor.
        for diag in &params.context.diagnostics {
            if diag.source.as_deref() != Some(SOURCE) {
                continue;
            }
            let suggestion = diag
                .data
                .as_ref()
                .and_then(|d| d.get("suggestion"))
                .and_then(|s| s.as_str())
                .filter(|s| !s.is_empty());
            if let Some(suggestion) = suggestion {
                let edit = TextEdit {
                    range: diag.range,
                    new_text: suggestion.to_string(),
                };
                let changes = HashMap::from([(uri.clone(), vec![edit])]);
                actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                    title: format!("Apply: {}", one_line(suggestion, 60)),
                    kind: Some(CodeActionKind::QUICKFIX),
                    diagnostics: Some(vec![diag.clone()]),
                    edit: Some(WorkspaceEdit {
                        changes: Some(changes),
                        ..Default::default()
                    }),
                    ..Default::default()
                }));
            }
        }

        // Manual triggers. `range` is forwarded to the command verbatim; an empty
        // range means "the whole document".
        let check_title = if has_selection {
            "Check writing in selection (grammar & style)"
        } else {
            "Check writing in document (grammar & style)"
        };
        actions.push(command_action(
            check_title,
            CHECK_COMMAND,
            vec![json!(uri), json!(range)],
        ));

        if has_selection {
            actions.push(command_action(
                "Rephrase selection…",
                REPHRASE_COMMAND,
                vec![json!(uri), json!(range)],
            ));
        }

        Ok(Some(actions))
    }

    async fn execute_command(
        &self,
        params: ExecuteCommandParams,
    ) -> tower_lsp::jsonrpc::Result<Option<serde_json::Value>> {
        let Some((uri, range)) = parse_command_args(&params.arguments) else {
            return Ok(None);
        };
        match params.command.as_str() {
            CHECK_COMMAND => self.run_check(uri, range).await,
            REPHRASE_COMMAND => self.run_rephrase(uri, range).await,
            _ => {}
        }
        Ok(None)
    }
}

impl Backend {
    /// Run a grammar/style check over the selection (or whole document) and
    /// publish the resulting diagnostics.
    async fn run_check(&self, uri: Url, range: Range) {
        let Some((llm, config)) = self.snapshot() else {
            return;
        };

        // Pull out the text to check and the byte window it lives in.
        let (full_text, window) = {
            let state = self.state.lock().unwrap();
            let Some(text) = state.documents.get(&uri) else {
                return;
            };
            let full = text.clone();
            let window = if range.start == range.end {
                (0, full.len())
            } else {
                LineIndex::new(&full).byte_range(range)
            };
            (full, window)
        };
        let (start, end) = window;
        let slice = &full_text[start..end.min(full_text.len())];

        if slice.trim().is_empty() {
            self.notify(MessageType::INFO, "helix-style: nothing to check")
                .await;
            return;
        }
        if slice.chars().count() > config.max_input_chars {
            self.notify(
                MessageType::WARNING,
                &format!(
                    "helix-style: input exceeds max-input-chars ({}); narrow the selection",
                    config.max_input_chars
                ),
            )
            .await;
            return;
        }

        self.notify(MessageType::INFO, "helix-style: checking…")
            .await;

        let (summary, issues) = match llm
            .check(
                slice,
                &config.style_profile,
                config.extra_instructions.as_deref(),
            )
            .await
        {
            Ok(result) => result,
            Err(e) => {
                self.notify(MessageType::ERROR, &format!("helix-style: {e}"))
                    .await;
                return;
            }
        };

        let index = LineIndex::new(&full_text);
        let severity = config.diagnostic_severity();
        let mut diagnostics = Vec::new();
        let mut unlocated = 0usize;
        for issue in &issues {
            match locate(&full_text, &issue.quote, start, end) {
                Some((qs, qe)) => {
                    diagnostics.push(issue_to_diagnostic(issue, &index, qs, qe, severity))
                }
                None => unlocated += 1,
            }
        }

        self.client
            .publish_diagnostics(uri, diagnostics.clone(), None)
            .await;

        self.show_summary(&summary, diagnostics.len(), unlocated)
            .await;
    }

    /// Present the check's compact evaluation as a dismissable popup. Helix
    /// renders a `window/showMessageRequest` carrying action items as a `Select`
    /// popup titled with the message; the reply is ignored.
    async fn show_summary(&self, summary: &str, located: usize, unlocated: usize) {
        let counts = match (located, unlocated) {
            (0, 0) => "no issues highlighted".to_string(),
            (n, 0) => format!("{n} issue(s) highlighted"),
            (n, u) => format!("{n} issue(s) highlighted, {u} could not be located"),
        };
        let summary = summary.trim();
        let message = if summary.is_empty() {
            format!("helix-style — {counts}")
        } else {
            format!("helix-style — {summary}\n({counts})")
        };
        let _ = self
            .client
            .show_message_request(
                MessageType::INFO,
                message,
                Some(vec![MessageActionItem {
                    title: "OK".to_string(),
                    properties: HashMap::new(),
                }]),
            )
            .await;
    }

    /// Ask the LLM for rephrasings of the selection, let the user pick one via a
    /// message-request menu, and apply it as a server-initiated edit.
    async fn run_rephrase(&self, uri: Url, range: Range) {
        if range.start == range.end {
            return; // rephrase only applies to a real selection
        }
        let Some((llm, config)) = self.snapshot() else {
            return;
        };

        let text = {
            let state = self.state.lock().unwrap();
            let Some(text) = state.documents.get(&uri) else {
                return;
            };
            let (s, e) = LineIndex::new(text).byte_range(range);
            text[s..e.min(text.len())].to_string()
        };

        if text.trim().is_empty() {
            return;
        }
        if text.chars().count() > config.max_input_chars {
            self.notify(
                MessageType::WARNING,
                "helix-style: selection too large to rephrase",
            )
            .await;
            return;
        }

        self.notify(MessageType::INFO, "helix-style: rephrasing…")
            .await;

        let alternatives = match llm
            .rephrase(
                &text,
                config.rephrase_options,
                &config.style_profile,
                config.extra_instructions.as_deref(),
            )
            .await
        {
            Ok(alts) => alts,
            Err(e) => {
                self.notify(MessageType::ERROR, &format!("helix-style: {e}"))
                    .await;
                return;
            }
        };

        // The Select popup is only as wide as its *message*, and the option menu
        // is clipped to that width — so we put each full rephrasing (numbered,
        // whitespace-collapsed) in the message body, where it wraps across lines
        // and is fully readable. The menu items stay short numbered picks that map
        // back by their leading number.
        let mut message = String::from("helix-style — choose a rephrasing:\n");
        for (i, alt) in alternatives.iter().enumerate() {
            message.push_str(&format!("\n{}. {}\n", i + 1, one_line(alt, 400)));
        }
        let actions: Vec<MessageActionItem> = alternatives
            .iter()
            .enumerate()
            .map(|(i, alt)| MessageActionItem {
                title: format!("{}. {}", i + 1, one_line(alt, 60)),
                properties: HashMap::new(),
            })
            .collect();

        let chosen = self
            .client
            .show_message_request(MessageType::INFO, message, Some(actions))
            .await;

        let Ok(Some(item)) = chosen else {
            return; // dismissed or transport error
        };
        let Some(idx) = item
            .title
            .split_once('.')
            .and_then(|(n, _)| n.trim().parse::<usize>().ok())
            .map(|n| n.saturating_sub(1))
        else {
            return;
        };
        let Some(replacement) = alternatives.get(idx) else {
            return;
        };

        let edit = WorkspaceEdit {
            changes: Some(HashMap::from([(
                uri,
                vec![TextEdit {
                    range,
                    new_text: replacement.clone(),
                }],
            )])),
            ..Default::default()
        };
        if let Err(e) = self.client.apply_edit(edit).await {
            self.notify(
                MessageType::ERROR,
                &format!("helix-style: applying edit failed: {e}"),
            )
            .await;
        }
    }

    async fn notify(&self, typ: MessageType, message: &str) {
        // log_message keeps it in the LSP log; errors/warnings also pop up.
        self.client.log_message(typ, message.to_string()).await;
        if typ == MessageType::ERROR || typ == MessageType::WARNING {
            self.client.show_message(typ, message.to_string()).await;
        }
    }
}

/// Build a diagnostic for an issue whose quote was located at bytes `[qs, qe)`.
fn issue_to_diagnostic(
    issue: &Issue,
    index: &LineIndex,
    qs: usize,
    qe: usize,
    severity: DiagnosticSeverity,
) -> Diagnostic {
    let category = if issue.category.is_empty() {
        "style".to_string()
    } else {
        issue.category.clone()
    };
    let message = if issue.explanation.is_empty() {
        format!("[{category}] writing issue")
    } else {
        format!("[{category}] {}", issue.explanation)
    };
    Diagnostic {
        range: index.range(qs, qe),
        severity: Some(severity),
        code: Some(NumberOrString::String(category)),
        source: Some(SOURCE.to_string()),
        message,
        // The code-action handler reads the replacement back out of `data`.
        data: Some(json!({ "suggestion": issue.suggestion })),
        ..Default::default()
    }
}

/// Find `quote` in `text`, preferring an occurrence inside the checked window
/// `[start, end)`; fall back to the whole document. Returns the byte range.
fn locate(text: &str, quote: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    if quote.is_empty() {
        return None;
    }
    let end = end.min(text.len());
    if let Some(rel) = text.get(start..end).and_then(|w| w.find(quote)) {
        let s = start + rel;
        return Some((s, s + quote.len()));
    }
    text.find(quote).map(|s| (s, s + quote.len()))
}

fn command_action(
    title: &str,
    command: &str,
    arguments: Vec<serde_json::Value>,
) -> CodeActionOrCommand {
    CodeActionOrCommand::Command(Command {
        title: title.to_string(),
        command: command.to_string(),
        arguments: Some(arguments),
    })
}

/// Parse `[uri, range]` from command arguments.
fn parse_command_args(args: &[serde_json::Value]) -> Option<(Url, Range)> {
    let uri: Url = serde_json::from_value(args.first()?.clone()).ok()?;
    let range: Range = serde_json::from_value(args.get(1)?.clone()).ok()?;
    Some((uri, range))
}

/// Collapse whitespace/newlines and truncate for use in a menu label.
fn one_line(s: &str, max: usize) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max {
        collapsed
    } else {
        let truncated: String = collapsed.chars().take(max).collect();
        format!("{truncated}…")
    }
}

fn workspace_root(params: &InitializeParams) -> Option<PathBuf> {
    if let Some(folders) = &params.workspace_folders {
        if let Some(folder) = folders.first() {
            if let Ok(path) = folder.uri.to_file_path() {
                return Some(path);
            }
        }
    }
    #[allow(deprecated)]
    params.root_uri.as_ref().and_then(|u| u.to_file_path().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locate_prefers_window() {
        let text = "needle in the haystack and another needle here";
        let second = text.rfind("needle").unwrap();
        let (s, e) = locate(text, "needle", second, text.len()).unwrap();
        assert_eq!(s, second);
        assert_eq!(&text[s..e], "needle");
    }

    #[test]
    fn locate_falls_back_to_whole_doc() {
        let text = "the quote is earlier in the document";
        let (s, e) = locate(text, "quote", 20, text.len()).unwrap();
        assert_eq!(&text[s..e], "quote");
    }

    #[test]
    fn one_line_collapses_and_truncates() {
        assert_eq!(one_line("a\n  b   c", 80), "a b c");
        assert_eq!(one_line("abcdef", 3), "abc…");
    }

    #[test]
    fn parse_args_round_trip() {
        let uri = Url::parse("file:///tmp/x.md").unwrap();
        let range = Range::new(Position::new(1, 0), Position::new(2, 5));
        let args = vec![json!(uri), json!(range)];
        let (u, r) = parse_command_args(&args).unwrap();
        assert_eq!(u, uri);
        assert_eq!(r, range);
    }
}
