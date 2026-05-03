use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::Context;
use chrono::Utc;
use quiver_core::tool::{ToolMeta, ToolType};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct McpServers {
    #[serde(rename = "mcpServers", default)]
    servers: HashMap<String, McpEntry>,
}

#[derive(Debug, Deserialize)]
struct McpEntry {
    command: String,
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default)]
    env: Option<HashMap<String, String>>,
}

pub fn parse_mcp_servers(path: &Path) -> anyhow::Result<Vec<ToolMeta>> {
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let parsed: McpServers =
        serde_json::from_str(&raw).with_context(|| format!("parse JSON {}", path.display()))?;

    let now = Utc::now();
    let mut out = Vec::with_capacity(parsed.servers.len());
    for (name, entry) in parsed.servers {
        let args = entry.args.clone().unwrap_or_default();
        let invocation = if args.is_empty() {
            entry.command.clone()
        } else {
            format!("{} {}", entry.command, args.join(" "))
        };
        let env_count = entry.env.as_ref().map(|e| e.len()).unwrap_or(0);
        let long_desc = format!(
            "command: {}\nargs: {:?}\nenv vars: {}",
            entry.command, args, env_count
        );

        out.push(ToolMeta {
            id: format!("mcp:{name}"),
            r#type: ToolType::Mcp,
            name: name.clone(),
            source_repo: None,
            install_path: None,
            description: Some(format!("MCP server: {name} ({})", entry.command)),
            long_description: Some(long_desc),
            category: None,
            triggers: Vec::new(),
            examples: Vec::new(),
            invocation: Some(invocation),
            requires: Vec::new(),
            enabled: true,
            added_at: now,
            last_seen_at: now,
            last_used_at: None,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/mcp_servers.json")
    }

    #[test]
    fn parses_two_servers_from_fixture() {
        let metas = parse_mcp_servers(&fixture_path()).unwrap();
        assert_eq!(metas.len(), 2);
        assert_eq!(metas[0].id, "mcp:context7");
        assert_eq!(metas[0].r#type, ToolType::Mcp);
        assert_eq!(
            metas[0].invocation.as_deref(),
            Some("npx -y @context7/mcp-server")
        );
        assert_eq!(metas[1].id, "mcp:nocmd");
        assert_eq!(metas[1].invocation.as_deref(), Some("echo"));
    }
}
