//! Spell-check dictionary: a base Hunspell dictionary (via `zspell`) plus two
//! user-maintained word lists (project-local and personal/global).
//!
//! `zspell` dictionaries are immutable once built, so user-added words are NOT
//! rebuilt into the engine. Instead they live in in-memory `HashSet`s loaded
//! from the two word-list files; adding a word is an O(1) insert plus a file
//! append, with no dictionary rebuild.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Which word list a newly-added word should go into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Personal,
    Project,
}

pub struct Dictionary {
    zspell: zspell::Dictionary,
    /// Lowercased user words, for case-insensitive matching (matches zspell).
    personal: HashSet<String>,
    project: HashSet<String>,
    personal_path: PathBuf,
    project_path: Option<PathBuf>,
}

impl Dictionary {
    /// Build a dictionary from explicit `.aff`/`.dic` paths and the two
    /// word-list file paths (which need not exist yet).
    pub fn load(
        aff_path: &Path,
        dic_path: &Path,
        personal_path: PathBuf,
        project_path: Option<PathBuf>,
    ) -> Result<Self, String> {
        let aff = fs::read_to_string(aff_path)
            .map_err(|e| format!("reading {}: {e}", aff_path.display()))?;
        let dic = fs::read_to_string(dic_path)
            .map_err(|e| format!("reading {}: {e}", dic_path.display()))?;
        let zspell = zspell::builder()
            .config_str(&aff)
            .dict_str(&dic)
            .build()
            .map_err(|e| format!("building dictionary: {e}"))?;

        let personal = load_word_list(&personal_path);
        let project = project_path
            .as_deref()
            .map(load_word_list)
            .unwrap_or_default();

        Ok(Self {
            zspell,
            personal,
            project,
            personal_path,
            project_path,
        })
    }

    /// Is `word` spelled correctly (per the base dictionary or a user list)?
    pub fn is_correct(&self, word: &str) -> bool {
        let lower = word.to_lowercase();
        self.personal.contains(&lower)
            || self.project.contains(&lower)
            || self.zspell.check_word(word)
    }

    /// Up to `max` replacement suggestions for a misspelled word (best-effort;
    /// zspell's suggestion engine is edit-distance based and unstable).
    pub fn suggest(&self, word: &str, max: usize) -> Vec<String> {
        self.zspell
            .entry(word)
            .suggest()
            .unwrap_or_default()
            .into_iter()
            .take(max)
            .map(str::to_string)
            .collect()
    }

    /// Add `word` to the given list, persisting it to the backing file.
    /// Returns the path that was written.
    pub fn add_word(&mut self, word: &str, scope: Scope) -> io::Result<PathBuf> {
        let path = match scope {
            Scope::Personal => self.personal_path.clone(),
            Scope::Project => self.project_path.clone().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "no project dictionary (workspace root unknown)",
                )
            })?,
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        writeln!(file, "{word}")?;

        let set = match scope {
            Scope::Personal => &mut self.personal,
            Scope::Project => &mut self.project,
        };
        set.insert(word.to_lowercase());
        Ok(path)
    }
}

/// Read a word-list file into a set of lowercased words. Missing files and
/// `#`-comment / blank lines are ignored.
fn load_word_list(path: &Path) -> HashSet<String> {
    let Ok(contents) = fs::read_to_string(path) else {
        return HashSet::new();
    };
    contents
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.to_lowercase())
        .collect()
}

/// Standard locations to search for `<name>.aff` / `<name>.dic` pairs.
fn dictionary_search_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/usr/share/hunspell"),
        PathBuf::from("/usr/share/hunspell/dicts"),
        PathBuf::from("/usr/share/myspell"),
        PathBuf::from("/usr/share/myspell/dicts"),
        PathBuf::from("/usr/local/share/hunspell"),
    ];
    if let Some(cfg) = dirs::config_dir() {
        dirs.push(cfg.join("helix-spell").join("dicts"));
    }
    dirs
}

/// Resolve a dictionary `name` (e.g. `"en_US"`) to its `.aff`/`.dic` paths by
/// scanning the standard directories.
pub fn resolve_dictionary(name: &str) -> Option<(PathBuf, PathBuf)> {
    for dir in dictionary_search_dirs() {
        let aff = dir.join(format!("{name}.aff"));
        let dic = dir.join(format!("{name}.dic"));
        if aff.is_file() && dic.is_file() {
            return Some((aff, dic));
        }
    }
    None
}

/// The default personal (global) word-list path: `<config>/helix-spell/personal.dic`.
pub fn default_personal_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("helix-spell")
        .join("personal.dic")
}
