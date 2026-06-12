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

/// System prompt for the synonym suggestion. The user message carries the word
/// and the sentence it appears in; the model returns `{"synonyms": ["...", ...]}`
/// with at most `n` context-appropriate replacements.
pub fn synonyms_system(n: usize, profile: &str, extra: Option<&str>) -> String {
    let mut s = format!(
        "You are a precise thesaurus assistant. {rubric}\n\n\
         The user sends a single word or short phrase together with the sentence it \
         appears in. Suggest up to {n} synonyms or near-synonyms that fit the \
         meaning of the word IN THAT SPECIFIC CONTEXT and could grammatically \
         replace it in the sentence (match part of speech, number and inflection). \
         Order them best-fit first, avoid duplicates and the original word itself, \
         and never include explanations. Return fewer than {n} rather than padding \
         with poor fits. Reply with ONLY a JSON object of the form \
         {{\"synonyms\": [\"...\"]}} and nothing else.",
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

/// User message for the synonym task, pairing the selected `word` with the
/// `sentence` it occurs in so the model can judge fit in context.
pub fn synonyms_user(word: &str, sentence: &str) -> String {
    format!("Word: \"{word}\"\nSentence: \"{sentence}\"")
}

/// System prompt for the explain task. The model returns
/// `{"meaning": "...", "example": "..."}` — a clear definition plus a typical
/// usage example for the selected word(s).
pub fn explain_system(extra: Option<&str>) -> String {
    let mut s = String::from(
        "You are a knowledgeable, concise language tutor. The user sends a word, \
         phrase, or short passage. Explain what it means in plain language, and give \
         one natural example of how it is normally used. Return a JSON object with \
         two fields:\n\
         - \"meaning\": a clear explanation of the meaning of the selected text (1-3 \
         sentences). If it is a technical or domain term, say so and explain it for a \
         general reader.\n\
         - \"example\": one short, natural sentence that demonstrates how the \
         word(s) are normally used. If the selection is itself a full sentence, give \
         an example in a different context.\n\
         Reply with ONLY a JSON object of the form \
         {\"meaning\": \"...\", \"example\": \"...\"} and nothing else.",
    );
    s.push_str("\n\n");
    s.push_str(MARKUP_NOTE);
    if let Some(extra) = extra.map(str::trim).filter(|e| !e.is_empty()) {
        s.push_str("\n\nAdditional project-specific instructions:\n");
        s.push_str(extra);
    }
    s
}
