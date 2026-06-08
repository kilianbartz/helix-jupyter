//! The tower-lsp server: state machine that turns document events into
//! spelling diagnostics and serves quick-fix code actions.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::json;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use crate::config::Config;
use crate::dictionary::{self, Dictionary, Scope};
use crate::extract::{self, Language};
use crate::position::LineIndex;

const SOURCE: &str = "helix-spell";
const ADD_WORD_COMMAND: &str = "helix-spell.addWord";

struct Document {
    text: String,
    language: Language,
}

#[derive(Default)]
struct State {
    config: Config,
    dictionary: Option<Dictionary>,
    workspace_root: Option<PathBuf>,
    documents: HashMap<Url, Document>,
}

pub struct Backend {
    client: Client,
    state: Mutex<State>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            state: Mutex::new(State::default()),
        }
    }

    /// Recompute and publish diagnostics for a single document.
    async fn refresh(&self, uri: &Url) {
        let diagnostics = {
            let state = self.state.lock().unwrap();
            match (state.documents.get(uri), state.dictionary.as_ref()) {
                (Some(doc), Some(dict)) => check_document(doc, dict, &state.config),
                _ => return,
            }
        };
        self.client
            .publish_diagnostics(uri.clone(), diagnostics, None)
            .await;
    }

    /// Recompute and publish diagnostics for every open document (used after the
    /// dictionary changes).
    async fn refresh_all(&self) {
        let batches: Vec<(Url, Vec<Diagnostic>)> = {
            let state = self.state.lock().unwrap();
            let Some(dict) = state.dictionary.as_ref() else {
                return;
            };
            state
                .documents
                .iter()
                .map(|(uri, doc)| (uri.clone(), check_document(doc, dict, &state.config)))
                .collect()
        };
        for (uri, diagnostics) in batches {
            self.client
                .publish_diagnostics(uri, diagnostics, None)
                .await;
        }
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(
        &self,
        params: InitializeParams,
    ) -> tower_lsp::jsonrpc::Result<InitializeResult> {
        let workspace_root = workspace_root(&params);
        let config = Config::from_value(params.initialization_options);

        // Resolve the base dictionary (explicit paths win over a name lookup).
        let resolved = match (&config.aff_path, &config.dic_path) {
            (Some(aff), Some(dic)) => Some((PathBuf::from(aff), PathBuf::from(dic))),
            _ => dictionary::resolve_dictionary(&config.dictionary),
        };

        let project_path = workspace_root
            .as_ref()
            .map(|root| root.join(&config.project_dict_file));
        let personal_path = dictionary::default_personal_path();

        let dictionary = match resolved {
            Some((aff, dic)) => match Dictionary::load(&aff, &dic, personal_path, project_path) {
                Ok(d) => Some(d),
                Err(e) => {
                    self.client
                        .log_message(MessageType::ERROR, format!("helix-spell: {e}"))
                        .await;
                    None
                }
            },
            None => {
                self.client
                    .log_message(
                        MessageType::ERROR,
                        format!(
                            "helix-spell: could not find dictionary '{}' in the standard directories",
                            config.dictionary
                        ),
                    )
                    .await;
                None
            }
        };

        {
            let mut state = self.state.lock().unwrap();
            state.config = config;
            state.workspace_root = workspace_root;
            state.dictionary = dictionary;
        }

        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "helix-spell-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec![ADD_WORD_COMMAND.to_string()],
                    ..Default::default()
                }),
                ..Default::default()
            },
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "helix-spell-lsp ready")
            .await;
    }

    async fn shutdown(&self) -> tower_lsp::jsonrpc::Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let doc = params.text_document;
        let language = Language::detect(&doc.language_id)
            .or_else(|| uri_extension(&doc.uri).and_then(|e| Language::detect(&e)));
        let Some(language) = language else {
            return; // unsupported file type — ignore silently
        };
        {
            let mut state = self.state.lock().unwrap();
            state.documents.insert(
                doc.uri.clone(),
                Document {
                    text: doc.text,
                    language,
                },
            );
        }
        self.refresh(&doc.uri).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        // FULL sync: the final change carries the entire document text.
        if let Some(change) = params.content_changes.into_iter().last() {
            let mut state = self.state.lock().unwrap();
            if let Some(doc) = state.documents.get_mut(&uri) {
                doc.text = change.text;
            }
        }
        self.refresh(&uri).await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        self.refresh(&params.text_document.uri).await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        {
            let mut state = self.state.lock().unwrap();
            state.documents.remove(&uri);
        }
        // Clear diagnostics for the closed document.
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
    }

    async fn code_action(
        &self,
        params: CodeActionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<CodeActionResponse>> {
        let mut actions: Vec<CodeActionOrCommand> = Vec::new();
        let uri = params.text_document.uri;

        let state = self.state.lock().unwrap();
        let Some(dict) = state.dictionary.as_ref() else {
            return Ok(None);
        };
        let has_project = state.workspace_root.is_some();

        for diag in &params.context.diagnostics {
            let Some(word) = spelling_word(diag) else {
                continue;
            };

            // Replacement suggestions (best-effort).
            for suggestion in dict.suggest(&word, state.config.max_suggestions) {
                let edit = TextEdit {
                    range: diag.range,
                    new_text: suggestion.clone(),
                };
                let changes = HashMap::from([(uri.clone(), vec![edit])]);
                actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                    title: format!("Replace with \"{suggestion}\""),
                    kind: Some(CodeActionKind::QUICKFIX),
                    diagnostics: Some(vec![diag.clone()]),
                    edit: Some(WorkspaceEdit {
                        changes: Some(changes),
                        ..Default::default()
                    }),
                    ..Default::default()
                }));
            }

            // Add-to-dictionary actions.
            if has_project {
                actions.push(add_word_action(
                    format!("Add \"{word}\" to project dictionary"),
                    &word,
                    "project",
                ));
            }
            actions.push(add_word_action(
                format!("Add \"{word}\" to personal dictionary"),
                &word,
                "personal",
            ));
        }

        Ok((!actions.is_empty()).then_some(actions))
    }

    async fn execute_command(
        &self,
        params: ExecuteCommandParams,
    ) -> tower_lsp::jsonrpc::Result<Option<serde_json::Value>> {
        if params.command != ADD_WORD_COMMAND {
            return Ok(None);
        }
        let word = params.arguments.first().and_then(|v| v.as_str());
        let scope = params.arguments.get(1).and_then(|v| v.as_str());
        let (Some(word), Some(scope)) = (word, scope) else {
            return Ok(None);
        };
        let scope = match scope {
            "project" => Scope::Project,
            _ => Scope::Personal,
        };

        let result = {
            let mut state = self.state.lock().unwrap();
            match state.dictionary.as_mut() {
                Some(dict) => dict.add_word(word, scope),
                None => return Ok(None),
            }
        };

        match result {
            Ok(path) => {
                self.client
                    .log_message(
                        MessageType::INFO,
                        format!("helix-spell: added \"{word}\" to {}", path.display()),
                    )
                    .await;
                self.refresh_all().await;
            }
            Err(e) => {
                self.client
                    .show_message(MessageType::ERROR, format!("helix-spell: {e}"))
                    .await;
            }
        }
        Ok(None)
    }
}

/// Run the spell check over a document and produce diagnostics.
fn check_document(doc: &Document, dict: &Dictionary, config: &Config) -> Vec<Diagnostic> {
    let index = LineIndex::new(&doc.text);
    let severity = config.diagnostic_severity();
    extract::extract(doc.language, &doc.text, config.ignore_uppercase)
        .into_iter()
        .filter(|word| !dict.is_correct(&word.text))
        .map(|word| Diagnostic {
            range: index.range(word.start, word.end),
            severity: Some(severity),
            code: Some(NumberOrString::String("spelling".to_string())),
            source: Some(SOURCE.to_string()),
            message: format!("Unknown word: {}", word.text),
            // The code-action handler reads the word back out of `data`.
            data: Some(json!({ "word": word.text })),
            ..Default::default()
        })
        .collect()
}

/// If `diag` is one of ours, return the offending word.
fn spelling_word(diag: &Diagnostic) -> Option<String> {
    if diag.source.as_deref() != Some(SOURCE) {
        return None;
    }
    diag.data
        .as_ref()
        .and_then(|d| d.get("word"))
        .and_then(|w| w.as_str())
        .map(str::to_string)
}

fn add_word_action(title: String, word: &str, scope: &str) -> CodeActionOrCommand {
    CodeActionOrCommand::CodeAction(CodeAction {
        title: title.clone(),
        kind: Some(CodeActionKind::QUICKFIX),
        command: Some(Command {
            title,
            command: ADD_WORD_COMMAND.to_string(),
            arguments: Some(vec![json!(word), json!(scope)]),
        }),
        ..Default::default()
    })
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

fn uri_extension(uri: &Url) -> Option<String> {
    uri.path()
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, ext)| ext.to_string())
}
