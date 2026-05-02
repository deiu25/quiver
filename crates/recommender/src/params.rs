//! Shared recommend-pipeline tuning constants + helpers.
//!
//! Used by both the CLI `recommend` command and the MCP server so changes
//! land in one place.

pub const VEC_CANDIDATES: usize = 50;
pub const FTS_CANDIDATES: usize = 50;
pub const COS_WEIGHT: f32 = 0.6;
pub const FTS_WEIGHT: f32 = 0.4;

/// Tokenise on whitespace, double-quote each token (escaping internal quotes),
/// and OR-join. OR keeps recall when only some words match.
pub fn build_fts_query(task: &str) -> String {
    let toks: Vec<String> = task
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| {
            let cleaned = t.replace('"', "");
            format!("\"{cleaned}\"")
        })
        .collect();
    toks.join(" OR ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_yields_empty_query() {
        assert_eq!(build_fts_query(""), "");
        assert_eq!(build_fts_query("   "), "");
    }

    #[test]
    fn quotes_each_token_and_or_joins() {
        assert_eq!(
            build_fts_query("design tokens"),
            "\"design\" OR \"tokens\""
        );
    }

    #[test]
    fn strips_embedded_quotes() {
        assert_eq!(build_fts_query("ab\"cd"), "\"abcd\"");
    }
}
