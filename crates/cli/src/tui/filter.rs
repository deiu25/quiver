use toolhub_core::tool::{ToolMeta, ToolType};

/// Linear in-memory filter over the catalogued tools. Returns indices into
/// the input slice in stable order so the caller can render without cloning.
///
/// `query` is matched case-insensitively against name + description +
/// category + triggers. `type_filter` AND-combines.
pub fn apply(tools: &[ToolMeta], query: &str, type_filter: Option<ToolType>) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    tools
        .iter()
        .enumerate()
        .filter(|(_, m)| match type_filter {
            Some(t) => m.r#type == t,
            None => true,
        })
        .filter(|(_, m)| q.is_empty() || matches_query(m, &q))
        .map(|(i, _)| i)
        .collect()
}

fn matches_query(m: &ToolMeta, q: &str) -> bool {
    if m.name.to_lowercase().contains(q) {
        return true;
    }
    if m.description
        .as_deref()
        .map(|d| d.to_lowercase().contains(q))
        .unwrap_or(false)
    {
        return true;
    }
    if m.category
        .as_deref()
        .map(|c| c.to_lowercase().contains(q))
        .unwrap_or(false)
    {
        return true;
    }
    m.triggers.iter().any(|t| t.to_lowercase().contains(q))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn meta(id: &str, name: &str, desc: &str, ttype: ToolType, triggers: &[&str]) -> ToolMeta {
        let now = Utc::now();
        ToolMeta {
            id: id.into(),
            r#type: ttype,
            name: name.into(),
            source_repo: None,
            install_path: None,
            description: Some(desc.into()),
            long_description: None,
            category: None,
            triggers: triggers.iter().map(|s| (*s).into()).collect(),
            examples: vec![],
            invocation: None,
            requires: vec![],
            enabled: true,
            added_at: now,
            last_seen_at: now,
            last_used_at: None,
        }
    }

    fn fixture() -> Vec<ToolMeta> {
        vec![
            meta(
                "skill:design-md",
                "design-md",
                "Generate semantic design system",
                ToolType::Skill,
                &["design", "tokens"],
            ),
            meta(
                "skill:enhance-prompt",
                "enhance-prompt",
                "Transform vague UI ideas",
                ToolType::Skill,
                &["prompt"],
            ),
            meta(
                "plugin:caveman",
                "caveman",
                "Ultra-compressed communication",
                ToolType::Plugin,
                &["caveman"],
            ),
            meta(
                "mcp:ruflo",
                "ruflo",
                "Mega MCP server with 200 tools",
                ToolType::Mcp,
                &["mcp"],
            ),
            meta(
                "cli:codeburn",
                "codeburn",
                "Burn rate dashboard for sessions",
                ToolType::Cli,
                &["codeburn"],
            ),
        ]
    }

    #[test]
    fn empty_query_returns_all() {
        let tools = fixture();
        let out = apply(&tools, "", None);
        assert_eq!(out, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn substring_matches_case_insensitive() {
        let tools = fixture();
        let out = apply(&tools, "DESIGN", None);
        assert_eq!(out, vec![0]);
    }

    #[test]
    fn matches_against_triggers() {
        let tools = fixture();
        let out = apply(&tools, "tokens", None);
        assert_eq!(out, vec![0]);
    }

    #[test]
    fn type_filter_excludes_other_types() {
        let tools = fixture();
        let out = apply(&tools, "", Some(ToolType::Skill));
        assert_eq!(out, vec![0, 1]);
    }

    #[test]
    fn combined_substring_and_type_filter_intersects() {
        let tools = fixture();
        let out = apply(&tools, "prompt", Some(ToolType::Plugin));
        assert!(out.is_empty());
    }

    #[test]
    fn whitespace_query_treated_as_empty() {
        let tools = fixture();
        let out = apply(&tools, "   ", None);
        assert_eq!(out, vec![0, 1, 2, 3, 4]);
    }
}
