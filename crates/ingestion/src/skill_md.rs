use std::fs;
use std::path::Path;

use anyhow::{Context, anyhow};
use chrono::Utc;
use quiver_core::tool::{ToolMeta, ToolScope, ToolType};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Frontmatter {
    #[serde(default)]
    name: Option<String>,
    description: Option<String>,
    #[serde(rename = "allowed-tools", default)]
    allowed_tools: Option<Vec<String>>,
}

fn fallback_name(dir: &Path) -> String {
    dir.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}

pub fn parse_skill_dir(dir: &Path) -> anyhow::Result<ToolMeta> {
    let skill_md = dir.join("SKILL.md");
    let raw =
        fs::read_to_string(&skill_md).with_context(|| format!("read {}", skill_md.display()))?;

    let (fm_text, body) = split_frontmatter(&raw)
        .ok_or_else(|| anyhow!("missing YAML frontmatter in {}", skill_md.display()))?;

    let fm: Frontmatter = serde_yaml::from_str(fm_text)
        .with_context(|| format!("parse frontmatter in {}", skill_md.display()))?;

    let now = Utc::now();
    let name = fm.name.unwrap_or_else(|| fallback_name(dir));
    Ok(ToolMeta {
        id: format!("skill:{name}"),
        r#type: ToolType::Skill,
        name: name.clone(),
        source_repo: None,
        install_path: Some(dir.display().to_string()),
        description: fm.description,
        long_description: Some(body.to_string()),
        category: None,
        triggers: Vec::new(),
        examples: Vec::new(),
        invocation: None,
        requires: fm.allowed_tools.unwrap_or_default(),
        enabled: true,
        added_at: now,
        last_seen_at: now,
        last_used_at: None,
        scope: ToolScope::User,
        scope_root: None,
    })
}

fn split_frontmatter(raw: &str) -> Option<(&str, &str)> {
    let rest = raw
        .strip_prefix("---\n")
        .or_else(|| raw.strip_prefix("---\r\n"))?;
    for delim in ["\n---\n", "\n---\r\n"] {
        if let Some(end) = rest.find(delim) {
            let fm = &rest[..end];
            let body_start = end + delim.len();
            return Some((fm, &rest[body_start..]));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/skills/design-md")
    }

    #[test]
    fn parses_design_md_fixture() {
        let meta = parse_skill_dir(&fixture_dir()).unwrap();
        assert_eq!(meta.name, "design-md");
        assert_eq!(meta.id, "skill:design-md");
        assert_eq!(meta.r#type, ToolType::Skill);
        assert!(meta.enabled);
        let desc = meta.description.expect("description present");
        assert!(desc.contains("Stitch"), "description was: {desc:?}");
        let body = meta.long_description.expect("body present");
        assert!(
            body.contains("Design Systems Lead"),
            "body did not contain expected marker"
        );
    }

    #[test]
    fn split_frontmatter_handles_basic_doc() {
        let raw = "---\nname: x\ndescription: y\n---\nbody here\n";
        let (fm, body) = split_frontmatter(raw).unwrap();
        assert_eq!(fm, "name: x\ndescription: y");
        assert_eq!(body, "body here\n");
    }

    #[test]
    fn missing_frontmatter_returns_none() {
        assert!(split_frontmatter("no frontmatter here").is_none());
    }

    #[test]
    fn frontmatter_without_name_falls_back_to_dir_name() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("widget-maker");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: Builds widgets\n---\nbody\n",
        )
        .unwrap();
        let meta = parse_skill_dir(&skill_dir).unwrap();
        assert_eq!(meta.name, "widget-maker");
        assert_eq!(meta.id, "skill:widget-maker");
        assert_eq!(meta.description.as_deref(), Some("Builds widgets"));
    }
}
