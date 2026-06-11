//! System prompts for the two LLM tasks: reviewing prose for grammar/style
//! issues, and rephrasing a selection. The prompts demand strict JSON so the
//! backend can parse the reply deterministically.

/// Shared instruction: keep markup/commands intact and emit valid JSON. Appended
/// to both prompts so quotes/suggestions/rephrasings never mangle `\commands`.
const MARKUP_NOTE: &str = "Preserve all markup and commands exactly as written: \
     LaTeX/Typst commands (e.g. \\section{...}, \\cite{key}), math, inline code and \
     Markdown syntax must be kept verbatim and never altered, translated, or \
     dropped. Commands contain backslashes, so escape every backslash in the JSON \
     string values (write a literal \\ as \\\\) so the reply stays valid JSON.";

/// Rubric fragment selected by the configured `style-profile`.
fn profile_rubric(profile: &str) -> &'static str {
    match profile.to_ascii_lowercase().as_str() {
        "scientific" => {
            "The text is from a scientific/academic document. Prioritise concision, \
             precision and clarity. Flag wordiness, redundancy, hedging, vague \
             quantifiers, unnecessary nominalisations, passive voice where an active \
             form is clearer, and run-on sentences. Do not flag domain terminology, \
             citations, equations, or field-standard phrasing."
        }
        "casual" => {
            "The text is casual writing. Flag clear grammar and spelling-adjacent \
             mistakes and genuinely confusing phrasing, but tolerate an informal tone."
        }
        _ => {
            "Flag grammar mistakes, awkward or unclear phrasing, wordiness and \
             redundancy. Keep a neutral, professional tone in suggestions."
        }
    }
}

/// System prompt for the document/selection review. The model must return a JSON
/// object: `{"issues": [{"quote","category","explanation","suggestion"}]}`.
pub fn check_system(profile: &str, extra: Option<&str>) -> String {
    let mut s = format!(
        "You are a meticulous writing reviewer. {rubric}\n\n\
         Review the text the user sends. Return a JSON object with two fields:\n\
         - \"summary\": a very compact (1-2 sentence) overall assessment of the \
         passage's writing quality — its main strength and the single most \
         important thing to improve.\n\
         - \"issues\": a list of concrete problems. For each issue return:\n\
         - \"quote\": the EXACT, verbatim substring from the text that has the issue \
         (copy it character-for-character so it can be located; keep it short — a \
         phrase or single sentence, never a whole paragraph).\n\
         - \"category\": one of \"grammar\", \"style\", \"conciseness\", \"clarity\".\n\
         - \"explanation\": one concise sentence on what is wrong.\n\
         - \"suggestion\": a corrected rewrite of just the quoted span, or an empty \
         string if no direct replacement applies.\n\n\
         Rules: Only report real problems; if the text is fine, return an empty \
         issues list (but still fill in \"summary\"). Do not invent text that is not \
         present. Do not rewrite the whole document. Reply with ONLY a JSON object of \
         the form {{\"summary\": \"...\", \"issues\": [ ... ]}} and nothing else.",
        rubric = profile_rubric(profile)
    );
    s.push_str("\n\n");
    s.push_str(MARKUP_NOTE);
    if let Some(extra) = extra.map(str::trim).filter(|e| !e.is_empty()) {
        s.push_str("\n\nAdditional project-specific instructions:\n");
        s.push_str(extra);
    }
    s
}

/// System prompt for rephrasing a selection into `n` alternatives. The model must
/// return `{"alternatives": ["...", ...]}`.
pub fn rephrase_system(n: usize, profile: &str, extra: Option<&str>) -> String {
    let mut s = format!(
        "You are a writing assistant. {rubric}\n\n\
         The user sends a passage. Produce {n} distinct rephrasings of it that \
         preserve the original meaning while improving clarity and concision. Vary \
         the alternatives meaningfully (do not return near-duplicates). Preserve any \
         surrounding whitespace conventions but return only the rewritten passage \
         text itself, with no quotes, numbering, or commentary. Reply with ONLY a \
         JSON object of the form {{\"alternatives\": [\"...\"]}} containing {n} \
         strings and nothing else.",
        rubric = profile_rubric(profile),
        n = n
    );
    s.push_str("\n\n");
    s.push_str(MARKUP_NOTE);
    if let Some(extra) = extra.map(str::trim).filter(|e| !e.is_empty()) {
        s.push_str("\n\nAdditional project-specific instructions:\n");
        s.push_str(extra);
    }
    s
}
