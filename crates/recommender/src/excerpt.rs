//! Body excerpt helper for context-budget-aware delivery.
//!
//! Used by the Quiver Claude Code hooks: when we recommend a skill via
//! `UserPromptSubmit`, we also inject the SKILL.md body so the model has the
//! actual instructions inline. Bodies can be huge (some plugin skills run
//! 10k+ chars); we trim to a per-hook char cap on the nearest paragraph
//! boundary so we never split mid-sentence.

const TRUNCATION_MARKER: &str = "\n\n…[truncated, full body via mcp__quiver__info]";

/// Return at most `max_chars` characters from `body`, trimmed to the nearest
/// paragraph break (`\n\n`) when possible. Appends a truncation marker when
/// the input was longer than the cap.
///
/// Always strips leading whitespace so we don't waste budget on empty lines
/// the YAML frontmatter parser leaves behind.
pub fn excerpt(body: &str, max_chars: usize) -> String {
    let body = body.trim_start();
    if body.chars().count() <= max_chars {
        return body.to_string();
    }

    // Find the byte index that corresponds to `max_chars` chars (UTF-8 safe).
    let cut_byte = body
        .char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or(body.len());
    let head = &body[..cut_byte];

    // Prefer the last paragraph boundary inside the head; else last single
    // newline; else hard cut.
    let split_at = head
        .rfind("\n\n")
        .or_else(|| head.rfind('\n'))
        .unwrap_or(head.len());
    let trimmed = head[..split_at].trim_end();

    format!("{trimmed}{TRUNCATION_MARKER}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_body_passes_through_unchanged() {
        let body = "# Title\n\nshort body.";
        let out = excerpt(body, 1000);
        assert_eq!(out, body);
        assert!(!out.contains("truncated"));
    }

    #[test]
    fn leading_whitespace_is_stripped() {
        let body = "\n\n# Title\n\nbody";
        let out = excerpt(body, 1000);
        assert!(out.starts_with("# Title"));
    }

    #[test]
    fn long_body_is_trimmed_and_marked() {
        // 3 paragraphs of 50 chars each => ~150 chars. Cap at 80 → keep p1.
        let p1 = "a".repeat(50);
        let p2 = "b".repeat(50);
        let p3 = "c".repeat(50);
        let body = format!("{p1}\n\n{p2}\n\n{p3}");
        let out = excerpt(&body, 80);
        assert!(out.contains(&p1));
        assert!(!out.contains(&p3));
        assert!(out.contains("truncated"));
    }

    #[test]
    fn trims_at_paragraph_boundary_when_possible() {
        let body = "first paragraph here.\n\nsecond paragraph here.\n\nthird.";
        let out = excerpt(body, 30);
        assert!(out.starts_with("first paragraph here."));
        // Should NOT bleed into "second paragraph"
        assert!(!out.contains("second"));
    }

    #[test]
    fn falls_back_to_single_newline_if_no_paragraph_break() {
        let body = "line one\nline two\nline three line three line three line";
        let out = excerpt(body, 25);
        assert!(out.starts_with("line one"));
        assert!(out.contains("truncated"));
    }

    #[test]
    fn handles_unicode_safely() {
        // Each emoji is multi-byte; cap on chars, not bytes.
        let body = "🐍".repeat(100);
        let out = excerpt(&body, 10);
        // Snake-emoji body has no newlines → fall through to hard cut at the
        // char boundary; truncation marker appended.
        assert!(out.contains("truncated"));
        // Must not panic / produce invalid UTF-8.
        assert!(out.is_ascii() || out.chars().count() <= 10 + TRUNCATION_MARKER.chars().count());
    }
}
