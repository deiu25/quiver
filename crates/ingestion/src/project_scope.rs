//! Per-project skill discovery + on-the-fly upsert.
//!
//! Walks `<project_root>/.claude/skills/` for SKILL.md files, parses each
//! into a [`ToolMeta`], then rewrites the id to a project-scoped form
//! (`skill:proj:<root-hash>:<name>`) so two unrelated repos can both ship
//! a skill named "tdd-workflow" without clobbering each other in the
//! catalog. Stamps `scope = Project` and `scope_root = canonical(project_root)`
//! so `ProjectScopeReranker` can find them later.

use std::path::Path;

use anyhow::Context;
use quiver_core::tool::{ToolMeta, ToolScope};
use quiver_recommender::embed::Embedder;
use quiver_storage::tools;
use rusqlite::Connection;

use crate::persist::persist_tools;
use crate::{skill_md, walker};

/// Relative subpath under the project root that holds local skills.
pub const PROJECT_SKILLS_SUBDIR: &str = ".claude/skills";

/// Canonicalise `project_root` and return the absolute path. Returns `None`
/// when the path doesn't exist (canonicalize fails on missing dirs) — callers
/// treat that as "no per-project context available".
pub fn canonical_project_root(project_root: &Path) -> Option<String> {
    std::fs::canonicalize(project_root)
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
}

/// Stable 16-hex-char fingerprint of a canonical project path (FNV-1a 64-bit).
/// Non-cryptographic — the goal is just a deterministic, collision-resistant-
/// enough id component that lets project-scope tools avoid clobbering global
/// twins or project-scope twins from unrelated repos. Pure-std so we don't
/// pull a hash crate in for ~10 lines of work.
pub fn scope_root_hash(canonical_root: &str) -> String {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x100_0000_01b3;
    let mut h: u64 = FNV_OFFSET;
    for b in canonical_root.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    format!("{h:016x}")
}

/// Walk `<project_root>/.claude/skills/` and return one [`ToolMeta`] per
/// SKILL.md found. Each meta carries `scope = Project`, the canonicalised
/// root, and a project-scoped id of the form
/// `skill:proj:<root-hash>:<original-name>`.
///
/// Returns an empty vec if the directory is missing or contains no SKILL.md.
/// Parse failures are logged via `tracing::warn` and that skill is skipped —
/// per-project ingestion is best-effort and never blocks recommend.
pub fn discover_project_skills(project_root: &Path) -> Vec<ToolMeta> {
    let Some(canonical) = canonical_project_root(project_root) else {
        return Vec::new();
    };
    let skills_dir = Path::new(&canonical).join(PROJECT_SKILLS_SUBDIR);
    if !skills_dir.is_dir() {
        return Vec::new();
    }
    let hash = scope_root_hash(&canonical);
    let mut out = Vec::new();
    for dir in walker::discover_skill_dirs(&skills_dir) {
        match skill_md::parse_skill_dir(&dir) {
            Ok(mut meta) => {
                // Rewrite id with project-scope prefix so the global
                // `skill:<name>` row is never clobbered.
                meta.id = format!("skill:proj:{hash}:{}", meta.name);
                meta.scope = ToolScope::Project;
                meta.scope_root = Some(canonical.clone());
                out.push(meta);
            },
            Err(err) => {
                tracing::warn!(path = %dir.display(), "skip project skill: {err:#}");
            },
        }
    }
    out
}

/// Cheap idempotent ingestion: discover project skills, skip any whose
/// SKILL.md body matches the previously-stored `long_description` (via byte
/// comparison through `tools::get`), and run [`persist_tools`] on the delta.
///
/// Returns the number of metas that were actually upserted (i.e. new or
/// content-changed). Pass-through when the project has no `.claude/skills/`
/// or the canonicalisation fails — never errors out the caller's recommend.
pub fn upsert_project_skills(
    conn: &Connection,
    embedder: &Embedder,
    project_root: &Path,
) -> anyhow::Result<usize> {
    let metas = discover_project_skills(project_root);
    if metas.is_empty() {
        return Ok(0);
    }

    let mut delta: Vec<ToolMeta> = Vec::new();
    for meta in metas {
        let existing = tools::get(conn, &meta.id).context("load existing project skill")?;
        let changed = match existing {
            None => true,
            Some(prev) => {
                prev.long_description != meta.long_description
                    || prev.description != meta.description
                    || prev.requires != meta.requires
                    || prev.scope_root != meta.scope_root
            },
        };
        if changed {
            delta.push(meta);
        }
    }

    if delta.is_empty() {
        return Ok(0);
    }

    persist_tools(conn, embedder, &delta).context("persist project-scope tools")?;
    Ok(delta.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_skill(root: &Path, name: &str, body: &str) {
        let skill_dir = root.join(PROJECT_SKILLS_SUBDIR).join(name);
        fs::create_dir_all(&skill_dir).unwrap();
        let content = format!(
            "---\nname: {name}\ndescription: {name} skill local to the project\n---\n{body}\n"
        );
        fs::write(skill_dir.join("SKILL.md"), content).unwrap();
    }

    #[test]
    fn discover_returns_empty_when_dir_missing() {
        let dir = tempfile::tempdir().unwrap();
        let metas = discover_project_skills(dir.path());
        assert!(metas.is_empty());
    }

    #[test]
    fn discover_tags_project_scope_and_root() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "myproj-tdd", "Run cargo test first.");
        let metas = discover_project_skills(dir.path());
        assert_eq!(metas.len(), 1);
        let m = &metas[0];
        assert_eq!(m.scope, ToolScope::Project);
        assert_eq!(m.name, "myproj-tdd");
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(m.scope_root.as_deref(), canonical.to_str());
        assert!(
            m.id.starts_with("skill:proj:"),
            "id should be project-scoped, got {}",
            m.id
        );
        assert!(
            m.id.ends_with(":myproj-tdd"),
            "id should end with original name, got {}",
            m.id
        );
    }

    #[test]
    fn discover_keeps_distinct_ids_per_project_root() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        write_skill(a.path(), "shared-skill", "from project A");
        write_skill(b.path(), "shared-skill", "from project B");

        let ma = discover_project_skills(a.path());
        let mb = discover_project_skills(b.path());
        assert_eq!(ma.len(), 1);
        assert_eq!(mb.len(), 1);
        assert_ne!(
            ma[0].id, mb[0].id,
            "different project roots must produce different ids"
        );
        assert_ne!(ma[0].scope_root, mb[0].scope_root);
    }

    #[test]
    fn scope_root_hash_is_deterministic_and_short() {
        let h1 = scope_root_hash("/tmp/example");
        let h2 = scope_root_hash("/tmp/example");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16); // 8 bytes hex = 16 chars
        assert_ne!(scope_root_hash("/tmp/a"), scope_root_hash("/tmp/b"));
    }

    #[test]
    fn project_skill_id_collision_safe_with_global() {
        // A globally installed `skill:python-testing` and a project skill
        // also named `python-testing` must produce different ids.
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "python-testing", "Project flavour.");
        let metas = discover_project_skills(dir.path());
        assert_ne!(metas[0].id, "skill:python-testing");
    }
}
