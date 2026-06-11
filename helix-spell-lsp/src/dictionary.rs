//! Spell-check dictionary: one or more base Hunspell dictionaries (via
//! `zspell`) plus two user-maintained word lists (project-local and
//! personal/global). With multiple base dictionaries a word is correct if
//! *any* of them knows it (mixed-language mode).
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

/// One named, resolved Hunspell dictionary to load.
#[derive(Debug, Clone)]
pub struct DictSpec {
    pub name: String,
    pub aff: PathBuf,
    pub dic: PathBuf,
}

struct NamedEngine {
    name: String,
    engine: zspell::Dictionary,
}

pub struct Dictionary {
    engines: Vec<NamedEngine>,
    /// Lowercased user words, for case-insensitive matching (matches zspell).
    personal: HashSet<String>,
    project: HashSet<String>,
    personal_path: PathBuf,
    project_path: Option<PathBuf>,
}

impl Dictionary {
    /// Build a dictionary from the given specs and the two word-list file
    /// paths (which need not exist yet). Specs that fail to load are skipped
    /// and reported in the returned warning list; it is an error only if no
    /// dictionary loads at all.
    pub fn load(
        specs: &[DictSpec],
        personal_path: PathBuf,
        project_path: Option<PathBuf>,
    ) -> Result<(Self, Vec<String>), String> {
        let mut engines = Vec::new();
        let mut failures = Vec::new();
        for spec in specs {
            match load_engine(spec) {
                Ok(engine) => engines.push(NamedEngine {
                    name: spec.name.clone(),
                    engine,
                }),
                Err(e) => failures.push(format!("skipped dictionary '{}': {e}", spec.name)),
            }
        }
        if engines.is_empty() {
            return Err(if failures.is_empty() {
                "no dictionaries configured".to_string()
            } else {
                failures.join("; ")
            });
        }

        let personal = load_word_list(&personal_path);
        let project = project_path
            .as_deref()
            .map(load_word_list)
            .unwrap_or_default();

        Ok((
            Self {
                engines,
                personal,
                project,
                personal_path,
                project_path,
            },
            failures,
        ))
    }

    /// Names of the successfully loaded base dictionaries, in load order.
    pub fn loaded_names(&self) -> Vec<&str> {
        self.engines.iter().map(|e| e.name.as_str()).collect()
    }

    /// Is `word` spelled correctly (per any base dictionary or a user list)?
    pub fn is_correct(&self, word: &str) -> bool {
        let lower = word.to_lowercase();
        self.personal.contains(&lower)
            || self.project.contains(&lower)
            || self.engines.iter().any(|e| e.engine.check_word(word))
    }

    /// Up to `max` replacement suggestions for a misspelled word, merged
    /// across all base dictionaries (best-effort; zspell's suggestion engine
    /// is edit-distance based and unstable).
    pub fn suggest(&self, word: &str, max: usize) -> Vec<String> {
        let lists: Vec<Vec<String>> = self
            .engines
            .iter()
            .map(|e| {
                e.engine
                    .entry(word)
                    .suggest()
                    .unwrap_or_default()
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            })
            .collect();
        interleave(lists, max)
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

/// Load one zspell engine from a spec's `.aff`/`.dic` files.
fn load_engine(spec: &DictSpec) -> Result<zspell::Dictionary, String> {
    let aff = fs::read_to_string(&spec.aff)
        .map_err(|e| format!("reading {}: {e}", spec.aff.display()))?;
    let dic = fs::read_to_string(&spec.dic)
        .map_err(|e| format!("reading {}: {e}", spec.dic.display()))?;
    build_engine(&aff, &dic)
}

/// Build a zspell engine from in-memory `.aff`/`.dic` contents.
fn build_engine(aff: &str, dic: &str) -> Result<zspell::Dictionary, String> {
    zspell::builder()
        .config_str(aff)
        .dict_str(dic)
        .build()
        .map_err(|e| format!("building dictionary: {e}"))
}

/// Round-robin merge of per-dictionary suggestion lists: all rank-1
/// suggestions (in dictionary order) before any rank-2 suggestion, deduped,
/// capped at `max`.
fn interleave(lists: Vec<Vec<String>>, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let rounds = lists.iter().map(Vec::len).max().unwrap_or(0);
    'outer: for i in 0..rounds {
        for list in &lists {
            if let Some(s) = list.get(i) {
                if seen.insert(s.clone()) {
                    out.push(s.clone());
                    if out.len() == max {
                        break 'outer;
                    }
                }
            }
        }
    }
    out
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

/// Resolve each name to a [`DictSpec`]; unresolvable names become warning
/// strings instead of failing the whole set.
pub fn resolve_specs(names: &[String]) -> (Vec<DictSpec>, Vec<String>) {
    let mut specs = Vec::new();
    let mut warnings = Vec::new();
    for name in names {
        match resolve_dictionary(name) {
            Some((aff, dic)) => specs.push(DictSpec {
                name: name.clone(),
                aff,
                dic,
            }),
            None => warnings.push(format!(
                "could not find dictionary '{name}' in the standard directories"
            )),
        }
    }
    (specs, warnings)
}

/// The default personal (global) word-list path: `<config>/helix-spell/personal.dic`.
pub fn default_personal_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("helix-spell")
        .join("personal.dic")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `Dictionary` from in-memory engines, bypassing all file IO.
    fn from_engines(engines: Vec<(&str, zspell::Dictionary)>) -> Dictionary {
        Dictionary {
            engines: engines
                .into_iter()
                .map(|(name, engine)| NamedEngine {
                    name: name.to_string(),
                    engine,
                })
                .collect(),
            personal: HashSet::new(),
            project: HashSet::new(),
            personal_path: PathBuf::from("/nonexistent/personal.dic"),
            project_path: None,
        }
    }

    fn english() -> zspell::Dictionary {
        build_engine("SET UTF-8\n", "2\nhello\nworld").unwrap()
    }

    fn german() -> zspell::Dictionary {
        build_engine("SET UTF-8\n", "2\nhallo\nwelt").unwrap()
    }

    #[test]
    fn mixed_mode_accepts_words_from_any_dictionary() {
        let dict = from_engines(vec![("en", english()), ("de", german())]);
        assert!(dict.is_correct("hello")); // first dictionary only
        assert!(dict.is_correct("hallo")); // second dictionary only
        assert!(!dict.is_correct("bonjour")); // neither
    }

    #[test]
    fn user_word_lists_apply_in_any_mode() {
        let mut dict = from_engines(vec![("en", english())]);
        assert!(!dict.is_correct("bonjour"));
        dict.personal.insert("bonjour".to_string());
        assert!(dict.is_correct("bonjour"));
    }

    #[test]
    fn loaded_names_reports_load_order() {
        let dict = from_engines(vec![("en_US", english()), ("de_DE", german())]);
        assert_eq!(dict.loaded_names(), vec!["en_US", "de_DE"]);
    }

    #[test]
    fn interleave_round_robins_dedupes_and_caps() {
        let lists = vec![
            vec!["a1".to_string(), "a2".to_string(), "a3".to_string()],
            vec!["b1".to_string(), "a1".to_string()],
        ];
        // Rank-1 entries from every list come first; the duplicate "a1" in
        // the second list is dropped.
        assert_eq!(interleave(lists.clone(), 10), vec!["a1", "b1", "a2", "a3"]);
        assert_eq!(interleave(lists, 2), vec!["a1", "b1"]);
        assert!(interleave(vec![], 5).is_empty());
    }

    #[test]
    fn single_list_interleave_is_take_max() {
        let lists = vec![vec!["x".to_string(), "y".to_string(), "z".to_string()]];
        assert_eq!(interleave(lists, 2), vec!["x", "y"]);
    }

    #[test]
    fn resolve_specs_turns_unknown_names_into_warnings() {
        let (specs, warnings) = resolve_specs(&["definitely_not_a_dictionary_xx_XX".to_string()]);
        assert!(specs.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("definitely_not_a_dictionary_xx_XX"));
    }

    #[test]
    fn load_skips_broken_specs_and_errors_only_when_none_load() {
        let bogus = DictSpec {
            name: "bogus".to_string(),
            aff: PathBuf::from("/nonexistent/bogus.aff"),
            dic: PathBuf::from("/nonexistent/bogus.dic"),
        };
        assert!(
            Dictionary::load(std::slice::from_ref(&bogus), default_personal_path(), None).is_err()
        );
        assert!(Dictionary::load(&[], default_personal_path(), None).is_err());

        // One good spec next to a broken one: load succeeds with a warning.
        let dir = std::env::temp_dir().join(format!("helix-spell-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let aff = dir.join("tiny.aff");
        let dic = dir.join("tiny.dic");
        fs::write(&aff, "SET UTF-8\n").unwrap();
        fs::write(&dic, "1\nhello").unwrap();
        let good = DictSpec {
            name: "tiny".to_string(),
            aff,
            dic,
        };
        let (dict, warnings) =
            Dictionary::load(&[good, bogus], default_personal_path(), None).unwrap();
        assert_eq!(dict.loaded_names(), vec!["tiny"]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("bogus"));
        fs::remove_dir_all(&dir).ok();
    }
}
