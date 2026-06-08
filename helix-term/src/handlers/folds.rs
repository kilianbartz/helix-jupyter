use helix_core::line_ending::line_end_char_index;
use helix_event::register_hook;
use helix_view::{events::DocumentDidOpen, handlers::Handlers, DocumentId, Editor};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Default)]
struct FoldsFile {
    #[serde(default)]
    folds: Vec<FoldEntry>,
}

#[derive(Serialize, Deserialize)]
struct FoldEntry {
    path: String,
    /// Each element is `[signature_line, resume_line]` (0-indexed).
    /// `signature_line` is the visible first line of the fold; `resume_line`
    /// is the first line that becomes visible again after the fold.
    ranges: Vec<[usize; 2]>,
}

/// Per-document fold state captured on the main thread before the async save.
pub(crate) struct DocFoldSnapshot {
    path_key: String,
    is_modified: bool,
    ranges: Vec<[usize; 2]>,
}

/// Capture the fold state of all open documents. Cheap — only reads in-memory
/// state; no file I/O. Call on the main thread before handing off to a
/// blocking task.
pub(crate) fn snapshot_folds(editor: &Editor) -> Vec<DocFoldSnapshot> {
    let (workspace_root, _) = helix_loader::find_workspace();
    let mut snapshots = Vec::new();
    for doc in editor.documents() {
        let Some(doc_path) = doc.path() else {
            continue;
        };
        let Ok(rel_path) = doc_path.strip_prefix(&workspace_root) else {
            continue;
        };
        let path_key = rel_path.to_string_lossy().into_owned();
        let is_modified = doc.is_modified();
        let text = doc.text().slice(..);
        let ranges = doc
            .folds()
            .iter()
            .map(|fold| [text.char_to_line(fold.start), fold.end_line])
            .collect();
        snapshots.push(DocFoldSnapshot {
            path_key,
            is_modified,
            ranges,
        });
    }
    snapshots
}

/// Merge the snapshot into the on-disk folds file. Blocking — call from
/// `tokio::task::spawn_blocking`.
pub(crate) fn flush_folds_to_disk(snapshots: Vec<DocFoldSnapshot>) -> anyhow::Result<()> {
    let folds_path = helix_loader::workspace_folds_file();

    // Load the existing file so we can preserve entries for modified (unsaved)
    // documents — their on-disk content hasn't changed, so previously saved
    // folds are still valid.
    let mut existing: Vec<FoldEntry> = std::fs::read_to_string(&folds_path)
        .ok()
        .and_then(|s| toml::from_str::<FoldsFile>(&s).ok())
        .map(|f| f.folds)
        .unwrap_or_default();

    for snap in snapshots {
        if snap.is_modified {
            // Document has unsaved changes — fold positions in memory may not
            // correspond to the saved file; leave the existing entry untouched.
            continue;
        }
        existing.retain(|e| e.path != snap.path_key);
        if !snap.ranges.is_empty() {
            existing.push(FoldEntry {
                path: snap.path_key,
                ranges: snap.ranges,
            });
        }
    }

    if existing.is_empty() {
        let _ = std::fs::remove_file(&folds_path);
        return Ok(());
    }

    if let Some(parent) = folds_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let content = toml::to_string_pretty(&FoldsFile { folds: existing })
        .map_err(|e| anyhow::anyhow!("Failed to serialize folds: {e}"))?;
    std::fs::write(&folds_path, content)?;
    Ok(())
}

fn load_folds_for_doc(editor: &mut Editor, doc_id: DocumentId) {
    let folds_path = helix_loader::workspace_folds_file();
    let (workspace_root, _) = helix_loader::find_workspace();

    // Clone the path so we release the immutable borrow before mutating.
    let path_key = {
        let Some(doc) = editor.documents.get(&doc_id) else {
            return;
        };
        let Some(doc_path) = doc.path() else {
            return;
        };
        let Ok(rel) = doc_path.strip_prefix(&workspace_root) else {
            return;
        };
        rel.to_string_lossy().into_owned()
    };

    let Ok(content) = std::fs::read_to_string(&folds_path) else {
        return;
    };
    let Ok(folds_file) = toml::from_str::<FoldsFile>(&content) else {
        return;
    };
    let Some(entry) = folds_file.folds.into_iter().find(|e| e.path == path_key) else {
        return;
    };

    let Some(doc) = editor.documents.get_mut(&doc_id) else {
        return;
    };
    let text_len_lines = doc.text().len_lines();
    for [sig_line, resume_line] in entry.ranges {
        if sig_line >= text_len_lines || resume_line > text_len_lines || sig_line >= resume_line {
            continue;
        }
        let text = doc.text().slice(..);
        let start = line_end_char_index(&text, sig_line);
        let end = if resume_line >= text.len_lines() {
            text.len_chars()
        } else {
            text.line_to_char(resume_line)
        };
        doc.add_fold(start, end);
    }
}

pub(super) fn register_hooks(_handlers: &Handlers) {
    register_hook!(move |event: &mut DocumentDidOpen<'_>| {
        load_folds_for_doc(event.editor, event.doc);
        Ok(())
    });
}
