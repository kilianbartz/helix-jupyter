//! The tower-lsp server: state machine that turns document events into
//! spelling diagnostics and serves quick-fix code actions.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::json;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use crate::config::{self, Config, ProjectConfig};
use crate::dictionary::{self, DictSpec, Dictionary, Scope};
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
    /// Dictionary summary for the "ready" log line, composed in `initialize`.
    status: String,
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

        let mut warnings: Vec<String> = Vec::new();
        let mut errors: Vec<String> = Vec::new();

        // Per-project config (`.spell.toml`) overrides the global one.
        let project_cfg = match workspace_root
            .as_ref()
            .map(|root| root.join(&config.project_config_file))
        {
            Some(path) => match ProjectConfig::load(&path) {
                Ok(pc) => pc.unwrap_or_default(),
                Err(e) => {
                    warnings.push(format!("ignoring project config: {e}"));
                    ProjectConfig::default()
                }
            },
            None => ProjectConfig::default(),
        };
        let restricted_by_project = project_cfg
            .language
            .as_ref()
            .is_some_and(|l| !l.trim().is_empty());
        let eff = config::effective_spelling(&config, &project_cfg);

        // An explicit aff/dic pair acts as one more dictionary, named after
        // the .dic file (and as the sole one when no names are configured).
        let explicit = match (&config.aff_path, &config.dic_path) {
            (Some(aff), Some(dic)) => {
                let dic = PathBuf::from(dic);
                let name = dic
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("custom")
                    .to_string();
                Some(DictSpec {
                    name,
                    aff: PathBuf::from(aff),
                    dic,
                })
            }
            _ => None,
        };

        let specs: Vec<DictSpec> = match &eff.language {
            // Single-language mode: load only the selected dictionary. It
            // need not appear in the configured list; never silently fall
            // back to mixed mode.
            Some(lang) => {
                let spec = match explicit.filter(|s| s.name == *lang) {
                    Some(spec) => Some(spec),
                    None => dictionary::resolve_dictionary(lang).map(|(aff, dic)| DictSpec {
                        name: lang.clone(),
                        aff,
                        dic,
                    }),
                };
                match spec {
                    Some(spec) => vec![spec],
                    None => {
                        errors.push(format!(
                            "could not find dictionary '{lang}' (selected by `language`) \
                             in the standard directories"
                        ));
                        Vec::new()
                    }
                }
            }
            // Mixed mode: load every configured dictionary.
            None => {
                let (mut specs, resolve_warnings) = dictionary::resolve_specs(&eff.names);
                warnings.extend(resolve_warnings);
                specs.extend(explicit);
                specs
            }
        };

        let project_path = workspace_root
            .as_ref()
            .map(|root| root.join(&config.project_dict_file));
        let personal_path = dictionary::default_personal_path();

        let dictionary = if specs.is_empty() {
            if errors.is_empty() {
                errors.push("no dictionaries could be resolved".to_string());
            }
            None
        } else {
            match Dictionary::load(&specs, personal_path, project_path) {
                Ok((d, load_warnings)) => {
                    warnings.extend(load_warnings);
                    Some(d)
                }
                Err(e) => {
                    errors.push(e);
                    None
                }
            }
        };

        for warning in &warnings {
            self.client
                .log_message(MessageType::WARNING, format!("helix-spell: {warning}"))
                .await;
        }
        for error in &errors {
            self.client
                .log_message(MessageType::ERROR, format!("helix-spell: {error}"))
                .await;
        }

        let status = match &dictionary {
            Some(d) => {
                let names = d.loaded_names().join(", ");
                match &eff.language {
                    Some(_) if restricted_by_project => format!(
                        "dictionary: {names} — restricted by {}",
                        config.project_config_file
                    ),
                    Some(_) => format!("dictionary: {names} — single-language mode"),
                    None if d.loaded_names().len() > 1 => {
                        format!("dictionaries: {names} — mixed mode")
                    }
                    None => format!("dictionary: {names}"),
                }
            }
            None => "no dictionary loaded".to_string(),
        };

        {
            let mut state = self.state.lock().unwrap();
            state.config = config;
            state.workspace_root = workspace_root;
            state.dictionary = dictionary;
            state.status = status;
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
        let status = self.state.lock().unwrap().status.clone();
        self.client
            .log_message(
                MessageType::INFO,
                format!("helix-spell-lsp ready ({status})"),
            )
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
