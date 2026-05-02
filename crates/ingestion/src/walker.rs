use std::collections::HashSet;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

/// Find every directory under `root` containing a `SKILL.md` file.
/// Paths are canonicalised so symlink twins (common under
/// `~/.claude/plugins/cache/`) collapse into a single entry.
/// Returns an empty vec if `root` does not exist.
pub fn discover_skill_dirs(root: &Path) -> Vec<PathBuf> {
    if !root.exists() {
        return Vec::new();
    }
    let mut seen = HashSet::new();
    WalkDir::new(root)
        .max_depth(8)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() && e.file_name() == "SKILL.md")
        .filter_map(|e| e.path().parent().map(PathBuf::from))
        .filter_map(|p| std::fs::canonicalize(&p).ok())
        .filter(|p| seen.insert(p.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/skills")
    }

    #[test]
    fn finds_design_md_under_fixtures() {
        let dirs = discover_skill_dirs(&fixtures_root());
        assert_eq!(dirs.len(), 1, "found dirs: {dirs:?}");
        assert!(dirs[0].ends_with("design-md"));
    }

    #[test]
    fn missing_root_returns_empty() {
        let dirs = discover_skill_dirs(Path::new("/definitely/not/here/xyzzy"));
        assert!(dirs.is_empty());
    }
}
