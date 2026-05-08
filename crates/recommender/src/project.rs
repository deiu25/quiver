//! Project-language detection from cwd marker files + skill→language mapping.
//!
//! Used by [`crate::rerank::LanguageReranker`] to penalise skills that
//! target a different ecosystem than the user's current project. A
//! `golang-patterns` skill scoring 0.85 in a Rust workspace is almost
//! certainly a false positive — neutralise it with a flat score penalty so
//! it falls out of `Mandatory` band without removing it entirely from the
//! result list.
//!
//! Detection is best-effort and fast: walks up at most 8 directory levels
//! from the cwd looking for marker files. A repo can be polyglot — every
//! detected language enters the set, and a skill that matches *any* of them
//! passes unchanged.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use quiver_core::tool::ToolMeta;

const MAX_WALKUP_DEPTH: usize = 8;

/// Coarse language buckets. Granular enough that "Rust workspace ≠ Go
/// project" but not so fine that "Node ≠ TypeScript" causes false demotions
/// in repos that ship both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    Go,
    Python,
    Node,
    Ts,
    Java,
    Kotlin,
    Csharp,
    Ruby,
    Php,
    Cpp,
    Swift,
    Dart,
}

impl Language {
    pub fn as_str(self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::Go => "go",
            Language::Python => "python",
            Language::Node => "node",
            Language::Ts => "typescript",
            Language::Java => "java",
            Language::Kotlin => "kotlin",
            Language::Csharp => "csharp",
            Language::Ruby => "ruby",
            Language::Php => "php",
            Language::Cpp => "cpp",
            Language::Swift => "swift",
            Language::Dart => "dart",
        }
    }
}

/// Walk up from `cwd` (capped at 8 levels) collecting language markers.
/// Returns an empty set when no markers exist — callers MUST treat empty as
/// "unknown, no penalty" rather than "everything is foreign".
pub fn detect_project_languages(cwd: &Path) -> HashSet<Language> {
    let mut langs = HashSet::new();
    let mut dir: Option<PathBuf> = Some(cwd.to_path_buf());
    let mut depth = 0;
    while let Some(d) = dir.take()
        && depth < MAX_WALKUP_DEPTH
    {
        scan_dir_for_markers(&d, &mut langs);
        dir = d.parent().map(Path::to_path_buf);
        depth += 1;
    }
    langs
}

fn scan_dir_for_markers(dir: &Path, langs: &mut HashSet<Language>) {
    let exists = |name: &str| dir.join(name).exists();
    let exists_with_ext = |ext: &str| {
        std::fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .any(|e| e.path().extension().and_then(|s| s.to_str()) == Some(ext))
            })
            .unwrap_or(false)
    };

    if exists("Cargo.toml") {
        langs.insert(Language::Rust);
    }
    if exists("go.mod") {
        langs.insert(Language::Go);
    }
    if exists("pyproject.toml")
        || exists("setup.py")
        || exists("requirements.txt")
        || exists("Pipfile")
    {
        langs.insert(Language::Python);
    }
    if exists("tsconfig.json") || exists("tsconfig.base.json") {
        langs.insert(Language::Ts);
    }
    if exists("package.json") && !langs.contains(&Language::Ts) {
        langs.insert(Language::Node);
    }
    if exists("pom.xml") || exists("build.gradle") {
        langs.insert(Language::Java);
    }
    if exists("build.gradle.kts") {
        langs.insert(Language::Kotlin);
    }
    if exists("global.json") || exists_with_ext("csproj") || exists_with_ext("sln") {
        langs.insert(Language::Csharp);
    }
    if exists("Gemfile") {
        langs.insert(Language::Ruby);
    }
    if exists("composer.json") {
        langs.insert(Language::Php);
    }
    if exists("CMakeLists.txt") {
        langs.insert(Language::Cpp);
    }
    if exists("Package.swift") {
        langs.insert(Language::Swift);
    }
    if exists("pubspec.yaml") {
        langs.insert(Language::Dart);
    }
}

/// Heuristic: pick a language tag for a skill, or `None` for
/// language-agnostic skills (e.g. "git-workflow", "code-review",
/// "context-budget"). Matches case-insensitively against `name`,
/// `category`, and `triggers`.
pub fn skill_language(meta: &ToolMeta) -> Option<Language> {
    let mut hay = String::with_capacity(256);
    push_lower(&mut hay, &meta.name);
    if let Some(c) = &meta.category {
        hay.push(' ');
        push_lower(&mut hay, c);
    }
    for t in &meta.triggers {
        hay.push(' ');
        push_lower(&mut hay, t);
    }

    // Order matters: more-specific tags first (Ts before Node, Kotlin
    // before Java) so a "kotlin-ktor" skill in a polyglot repo lands on
    // Kotlin instead of Java via the `gradle` substring.
    if contains_word(&hay, "typescript") || contains_word(&hay, "ts-") || contains_word(&hay, "tsx")
    {
        return Some(Language::Ts);
    }
    if contains_word(&hay, "kotlin")
        || contains_word(&hay, "ktor")
        || contains_word(&hay, "compose-multiplatform")
    {
        return Some(Language::Kotlin);
    }
    if contains_word(&hay, "rust") || contains_word(&hay, "cargo") {
        return Some(Language::Rust);
    }
    if contains_word(&hay, "golang")
        || contains_word(&hay, "go-")
        || contains_word(&hay, "go test")
        || contains_word(&hay, "gofmt")
    {
        return Some(Language::Go);
    }
    if contains_word(&hay, "python")
        || contains_word(&hay, "py-")
        || contains_word(&hay, "pep8")
        || contains_word(&hay, "pytest")
        || contains_word(&hay, "django")
        || contains_word(&hay, "flask")
        || contains_word(&hay, "fastapi")
        || contains_word(&hay, "pytorch")
    {
        return Some(Language::Python);
    }
    if contains_word(&hay, "nodejs")
        || contains_word(&hay, "npm")
        || contains_word(&hay, "pnpm")
        || contains_word(&hay, "yarn")
        || contains_word(&hay, "express")
        || contains_word(&hay, "nestjs")
        || contains_word(&hay, "nextjs")
        || contains_word(&hay, "nuxt")
    {
        return Some(Language::Node);
    }
    if contains_word(&hay, "java")
        || contains_word(&hay, "springboot")
        || contains_word(&hay, "jpa-")
        || contains_word(&hay, "gradle")
        || contains_word(&hay, "maven")
    {
        return Some(Language::Java);
    }
    if contains_word(&hay, "csharp")
        || contains_word(&hay, "dotnet")
        || contains_word(&hay, "c-sharp")
    {
        return Some(Language::Csharp);
    }
    if contains_word(&hay, "ruby") || contains_word(&hay, "rails") || contains_word(&hay, "rspec") {
        return Some(Language::Ruby);
    }
    if contains_word(&hay, "php")
        || contains_word(&hay, "laravel")
        || contains_word(&hay, "composer")
    {
        return Some(Language::Php);
    }
    if contains_word(&hay, "cpp") || contains_word(&hay, "c++") || contains_word(&hay, "cmake") {
        return Some(Language::Cpp);
    }
    if contains_word(&hay, "swift") || contains_word(&hay, "swiftui") || contains_word(&hay, "objc")
    {
        return Some(Language::Swift);
    }
    if contains_word(&hay, "dart") || contains_word(&hay, "flutter") {
        return Some(Language::Dart);
    }
    None
}

fn push_lower(out: &mut String, s: &str) {
    for ch in s.chars() {
        for low in ch.to_lowercase() {
            out.push(low);
        }
    }
}

/// Substring check that respects word-ish boundaries on both sides. Avoids
/// matching "java" inside "javascript". Operates on bytes — fine for the
/// ASCII-only language tag list above.
fn contains_word(hay: &str, needle: &str) -> bool {
    let mut start = 0;
    while let Some(pos) = hay[start..].find(needle) {
        let abs = start + pos;
        let before_ok = abs == 0 || !is_word_char_byte(hay.as_bytes()[abs - 1]);
        let end = abs + needle.len();
        let after_ok = end == hay.len() || !is_word_char_byte(hay.as_bytes()[end]);
        if before_ok && after_ok {
            return true;
        }
        start = abs + needle.len();
    }
    false
}

fn is_word_char_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use quiver_core::tool::{ToolMeta, ToolType};
    use std::fs;

    fn meta(name: &str, category: Option<&str>, triggers: &[&str]) -> ToolMeta {
        ToolMeta {
            id: format!("skill:{name}"),
            r#type: ToolType::Skill,
            name: name.to_string(),
            source_repo: None,
            install_path: None,
            description: None,
            long_description: None,
            category: category.map(str::to_string),
            triggers: triggers.iter().map(|s| s.to_string()).collect(),
            examples: vec![],
            invocation: None,
            requires: vec![],
            enabled: true,
            added_at: Utc::now(),
            last_seen_at: Utc::now(),
            last_used_at: None,
            scope: quiver_core::tool::ToolScope::User,
            scope_root: None,
        }
    }

    #[test]
    fn detect_rust_in_workspace() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        let langs = detect_project_languages(dir.path());
        assert!(langs.contains(&Language::Rust));
    }

    #[test]
    fn detect_python_in_pyproject() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("pyproject.toml"), "").unwrap();
        let langs = detect_project_languages(dir.path());
        assert!(langs.contains(&Language::Python));
    }

    #[test]
    fn detect_node_in_package_json_without_tsconfig() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();
        let langs = detect_project_languages(dir.path());
        assert!(langs.contains(&Language::Node));
        assert!(!langs.contains(&Language::Ts));
    }

    #[test]
    fn detect_ts_when_tsconfig_present() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();
        fs::write(dir.path().join("tsconfig.json"), "{}").unwrap();
        let langs = detect_project_languages(dir.path());
        assert!(langs.contains(&Language::Ts));
    }

    #[test]
    fn detect_walks_up_to_workspace_root() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b").join("c");
        fs::create_dir_all(&nested).unwrap();
        fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        let langs = detect_project_languages(&nested);
        assert!(langs.contains(&Language::Rust));
    }

    #[test]
    fn detect_returns_empty_for_no_markers() {
        let dir = tempfile::tempdir().unwrap();
        let langs = detect_project_languages(dir.path());
        assert!(langs.is_empty());
    }

    #[test]
    fn detect_polyglot_repo() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        fs::write(dir.path().join("go.mod"), "").unwrap();
        fs::write(dir.path().join("pyproject.toml"), "").unwrap();
        let langs = detect_project_languages(dir.path());
        assert!(langs.contains(&Language::Rust));
        assert!(langs.contains(&Language::Go));
        assert!(langs.contains(&Language::Python));
    }

    #[test]
    fn skill_language_golang_from_name() {
        assert_eq!(
            skill_language(&meta("golang-patterns", None, &[])),
            Some(Language::Go),
        );
    }

    #[test]
    fn skill_language_python_from_triggers() {
        assert_eq!(
            skill_language(&meta("server-patterns", None, &["pytest", "fastapi"])),
            Some(Language::Python),
        );
    }

    #[test]
    fn skill_language_rust_from_category() {
        assert_eq!(
            skill_language(&meta("borrow-checker-helper", Some("rust tooling"), &[])),
            Some(Language::Rust),
        );
    }

    #[test]
    fn skill_language_typescript_before_node() {
        let m = meta("typescript-reviewer", None, &["typescript", "nodejs"]);
        assert_eq!(skill_language(&m), Some(Language::Ts));
    }

    #[test]
    fn skill_language_none_for_language_agnostic() {
        assert_eq!(skill_language(&meta("git-workflow", None, &[])), None);
        assert_eq!(skill_language(&meta("code-review", None, &[])), None);
        assert_eq!(skill_language(&meta("context-budget", None, &[])), None);
    }

    #[test]
    fn skill_language_word_boundary_blocks_false_match() {
        let m = meta("javascript-toolkit", None, &[]);
        assert_ne!(skill_language(&m), Some(Language::Java));
    }
}
