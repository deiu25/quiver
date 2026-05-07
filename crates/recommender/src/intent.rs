//! Lexical intent classifier for user prompts.
//!
//! Strict-mode hooks score every prompt and emit a directive at
//! `Strong`/`Mandatory`. The score alone has no idea whether the user is
//! *asking* about something or *asking it to be done* — meta-questions like
//! "ce face X?" or "explain Y" get pushed to Mandatory just like a real
//! "implement X" request.
//!
//! This module runs *after* the policy ladder classifies score → band and
//! *before* the hook emits a directive. Question intent ⇒ downgrade to
//! `Silent` (suppress entirely). Analysis intent ⇒ downgrade to `Hint`
//! (still inject context, drop the directive).
//!
//! Pure-Rust, no regex dep. Two char-class checks + a small word list.
//! Latency is dominated by `prompt.chars().take(N).collect::<String>()` — well
//! under the hook's <30ms budget.

use crate::policy::Policy;

/// What the user appears to want from this prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// Imperative / task: "implement X", "fix Y", "refactor Z".
    Operational,
    /// Discussion / explanation: "explain X", "review Y", "analyze Z".
    Analysis,
    /// Interrogative: "what is X?", "how do I Y?", "ce face Z?".
    Question,
}

/// Question-leading words. EN + RO. Lower-case, alpha-only.
const QUESTION_WORDS: &[&str] = &[
    // EN
    "what", "how", "why", "when", "where", "which", "who", "whom", "whose", "can", "could",
    "should", "would", "does", "do", "is", "are", "was", "were", "will", "may", "might", "shall",
    // RO
    "ce", "cum", "cand", "când", "unde", "care", "cine", "poti", "poți", "poate", "oare", "este",
];

/// Multi-word question leads. Checked before single-word fallback.
const QUESTION_PHRASES: &[&str] = &[
    "de ce",
    "am putea",
    "ar trebui",
    "ce face",
    "ce este",
    "ce inseamna",
    "ce înseamnă",
    "cum sa",
    "cum să",
];

/// Analysis-leading words. EN + RO (with and without diacritics for safety).
const ANALYSIS_WORDS: &[&str] = &[
    // EN
    "explain",
    "analyze",
    "analyse",
    "review",
    "compare",
    "describe",
    "summarize",
    "summarise",
    "evaluate",
    "audit",
    "diagnose",
    "interpret",
    // RO
    "explica",
    "explică",
    "analizeaza",
    "analizează",
    "evalueaza",
    "evaluează",
    "compara",
    "compară",
    "descrie",
    "sumarizeaza",
    "sumarizează",
    "rezuma",
    "rezumă",
    "revizuieste",
    "revizuiește",
    "interpreteaza",
    "interpretează",
];

/// Classify a raw prompt. Order matters: Question first (strongest signal),
/// then Analysis, fallthrough Operational.
pub fn classify_intent(prompt: &str) -> Intent {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        return Intent::Operational;
    }

    if trimmed.ends_with('?') || trimmed.ends_with('？') {
        return Intent::Question;
    }

    let lower = leading_lower(trimmed, 64);

    if QUESTION_PHRASES.iter().any(|p| starts_with_word(&lower, p)) {
        return Intent::Question;
    }
    if QUESTION_WORDS.iter().any(|w| first_token_is(&lower, w)) {
        return Intent::Question;
    }
    if ANALYSIS_WORDS
        .iter()
        .any(|w| first_token_starts_with(&lower, w))
    {
        return Intent::Analysis;
    }

    Intent::Operational
}

/// Apply the score-band downgrade rule for an `(Policy, Intent)` pair.
///
/// `QUIVER_INTENT_FILTER=off` short-circuits to identity for debugging.
pub fn apply_downgrade(policy: Policy, intent: Intent) -> Policy {
    if !filter_enabled() {
        return policy;
    }
    match (policy, intent) {
        (_, Intent::Operational) => policy,
        (Policy::Silent, _) => Policy::Silent,
        (_, Intent::Question) => Policy::Silent,
        (Policy::Mandatory | Policy::Strong, Intent::Analysis) => Policy::Hint,
        (Policy::Hint, Intent::Analysis) => Policy::Hint,
    }
}

fn filter_enabled() -> bool {
    !matches!(
        std::env::var("QUIVER_INTENT_FILTER")
            .ok()
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "off" | "0" | "no" | "disabled",
    )
}

/// Lower-cased prefix of `s` capped at `max` chars (not bytes). Strips
/// leading punctuation but keeps spaces so token boundaries survive.
fn leading_lower(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(max);
    let mut taken = 0;
    let mut started = false;
    for ch in s.chars() {
        if !started && (ch.is_ascii_punctuation() || ch == '*' || ch == '#' || ch == '>') {
            continue;
        }
        started = true;
        if taken >= max {
            break;
        }
        for low in ch.to_lowercase() {
            out.push(low);
        }
        taken += 1;
    }
    out
}

/// Word-boundary-aware prefix match: `phrase` must be followed by a word
/// boundary (space, end-of-string, punctuation) so "ce" does not match
/// "cele".
fn starts_with_word(haystack: &str, phrase: &str) -> bool {
    if !haystack.starts_with(phrase) {
        return false;
    }
    haystack[phrase.len()..]
        .chars()
        .next()
        .map(|c| !c.is_alphanumeric() && c != '_')
        .unwrap_or(true)
}

/// True iff the first whitespace-delimited token of `haystack` equals `word`.
fn first_token_is(haystack: &str, word: &str) -> bool {
    let token = haystack.split_whitespace().next().unwrap_or("");
    let stripped = token.trim_end_matches(|c: char| !c.is_alphanumeric());
    stripped == word
}

/// True iff the first whitespace-delimited token of `haystack` starts with
/// `prefix`. Used for analysis verbs where suffixes like "-mi" / "-ne"
/// (Romanian clitics: "explică-mi") still count.
fn first_token_starts_with(haystack: &str, prefix: &str) -> bool {
    let token = haystack.split_whitespace().next().unwrap_or("");
    token.starts_with(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn question_trailing_qmark() {
        assert_eq!(classify_intent("the cake is a lie?"), Intent::Question);
    }

    #[test]
    fn question_en_what() {
        assert_eq!(
            classify_intent("what does this function do"),
            Intent::Question,
        );
    }

    #[test]
    fn question_en_how() {
        assert_eq!(
            classify_intent("how should I structure the cache"),
            Intent::Question,
        );
    }

    #[test]
    fn question_ro_ce() {
        assert_eq!(classify_intent("ce face funcția asta"), Intent::Question);
    }

    #[test]
    fn question_ro_cum() {
        assert_eq!(classify_intent("cum rezolvăm asta"), Intent::Question);
    }

    #[test]
    fn question_ro_de_ce() {
        assert_eq!(classify_intent("de ce nu merge"), Intent::Question);
    }

    #[test]
    fn question_word_boundary_blocks_substring_match() {
        assert_eq!(
            classify_intent("cele mai bune practici"),
            Intent::Operational,
        );
    }

    #[test]
    fn analysis_en_explain() {
        assert_eq!(
            classify_intent("explain the policy ladder"),
            Intent::Analysis,
        );
    }

    #[test]
    fn analysis_ro_analizeaza() {
        assert_eq!(
            classify_intent("analizează codul din hook.rs"),
            Intent::Analysis,
        );
    }

    #[test]
    fn analysis_ro_explica_with_clitic() {
        assert_eq!(
            classify_intent("explică-mi cum funcționează ingestion"),
            Intent::Analysis,
        );
    }

    #[test]
    fn operational_en_implement() {
        assert_eq!(
            classify_intent("implement an intent filter for the hook"),
            Intent::Operational,
        );
    }

    #[test]
    fn operational_ro_implementeaza() {
        assert_eq!(
            classify_intent("implementează filtrul de intenție"),
            Intent::Operational,
        );
    }

    #[test]
    fn operational_imperative_with_qmark_in_middle() {
        assert_eq!(
            classify_intent("fix the ? in the regex parser"),
            Intent::Operational,
        );
    }

    #[test]
    fn empty_prompt_is_operational() {
        assert_eq!(classify_intent(""), Intent::Operational);
        assert_eq!(classify_intent("   "), Intent::Operational);
    }

    #[test]
    fn strips_leading_markdown_artefacts() {
        assert_eq!(classify_intent("> what is happening?"), Intent::Question);
        assert_eq!(classify_intent("# Explain the build"), Intent::Analysis);
    }

    #[test]
    fn downgrade_mandatory_question_silent() {
        assert_eq!(
            apply_downgrade(Policy::Mandatory, Intent::Question),
            Policy::Silent,
        );
    }

    #[test]
    fn downgrade_strong_question_silent() {
        assert_eq!(
            apply_downgrade(Policy::Strong, Intent::Question),
            Policy::Silent,
        );
    }

    #[test]
    fn downgrade_mandatory_analysis_hint() {
        assert_eq!(
            apply_downgrade(Policy::Mandatory, Intent::Analysis),
            Policy::Hint,
        );
    }

    #[test]
    fn downgrade_strong_analysis_hint() {
        assert_eq!(
            apply_downgrade(Policy::Strong, Intent::Analysis),
            Policy::Hint,
        );
    }

    #[test]
    fn downgrade_operational_unchanged() {
        for p in [
            Policy::Silent,
            Policy::Hint,
            Policy::Strong,
            Policy::Mandatory,
        ] {
            assert_eq!(apply_downgrade(p, Intent::Operational), p);
        }
    }

    #[test]
    fn downgrade_silent_stays_silent() {
        for i in [Intent::Operational, Intent::Analysis, Intent::Question] {
            assert_eq!(apply_downgrade(Policy::Silent, i), Policy::Silent);
        }
    }

    #[test]
    fn env_off_skips_downgrade() {
        let key = "QUIVER_INTENT_FILTER";
        let prev = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, "off");
        }
        assert_eq!(
            apply_downgrade(Policy::Mandatory, Intent::Question),
            Policy::Mandatory,
        );
        unsafe {
            match prev {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}
