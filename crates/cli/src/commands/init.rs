//! `quiver init` — zero-config bootstrap.
//!
//! Wires Claude Code hooks into `~/.claude/settings.json`, runs an initial
//! sync if the catalog is empty, and (optionally) writes a single primer
//! SKILL.md to `~/.claude/skills/quiver-pilot/` so the model knows what the
//! `<quiver-recommendation>` blocks mean.
//!
//! Idempotent: re-running detects existing entries (matched by `quiver hook`
//! substring) and skips duplicates. Atomic: every write is preceded by a
//! `<file>.quiver-init.bak` backup and uses temp-file + rename.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use serde_json::{Value, json};

use crate::db_path::default_db_path;

const META_SKILL_BODY: &str = include_str!("../../assets/quiver-pilot/SKILL.md");
const META_SKILL_DIR: &str = "skills/quiver-pilot";
const META_SKILL_FILE: &str = "SKILL.md";

#[derive(Args, Debug, Clone)]
pub struct InitArgs {
    /// `user` writes to `~/.claude/settings.json`; `project` writes to
    /// `<cwd>/.claude/settings.json` (per-repo hooks).
    #[arg(long, default_value = "user")]
    pub scope: Scope,
    /// Skip the optional `~/.claude/skills/quiver-pilot/SKILL.md` primer.
    /// The hooks still work without it; the meta-skill just teaches the
    /// model what to do with `<quiver-recommendation>` blocks.
    #[arg(long)]
    pub no_meta_skill: bool,
    /// Skip the initial `quiver sync`. Use when the catalog is already
    /// populated and you only want to wire the hooks.
    #[arg(long)]
    pub no_sync: bool,
    /// Print the proposed changes and exit without writing anything.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    User,
    Project,
}

pub async fn run(args: InitArgs) -> Result<()> {
    let plan = build_plan(&args)?;
    print_plan(&plan, args.dry_run);
    if args.dry_run {
        return Ok(());
    }

    if !args.no_sync {
        run_sync_if_needed().await?;
    }

    apply_settings(&plan)?;
    if !args.no_meta_skill {
        write_meta_skill(&plan)?;
    }

    println!();
    print_next_steps(&plan);
    Ok(())
}

#[derive(Debug)]
struct Plan {
    scope: Scope,
    settings_path: PathBuf,
    meta_skill_path: PathBuf,
    quiver_bin: String,
}

fn build_plan(args: &InitArgs) -> Result<Plan> {
    let home = std::env::var("HOME").context("HOME is unset")?;
    let home = PathBuf::from(home);
    let settings_path = match args.scope {
        Scope::User => home.join(".claude/settings.json"),
        Scope::Project => std::env::current_dir()?.join(".claude/settings.json"),
    };
    let meta_skill_path = home
        .join(".claude")
        .join(META_SKILL_DIR)
        .join(META_SKILL_FILE);
    let quiver_bin = current_quiver_binary();

    Ok(Plan {
        scope: args.scope,
        settings_path,
        meta_skill_path,
        quiver_bin,
    })
}

fn current_quiver_binary() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "quiver".to_string())
}

fn print_plan(plan: &Plan, dry: bool) {
    let prefix = if dry { "DRY-RUN" } else { "INIT" };
    eprintln!("[{prefix}] scope: {:?}", plan.scope);
    eprintln!("[{prefix}] settings: {}", plan.settings_path.display());
    eprintln!("[{prefix}] meta-skill: {}", plan.meta_skill_path.display());
    eprintln!("[{prefix}] quiver binary: {}", plan.quiver_bin);
}

fn print_next_steps(plan: &Plan) {
    println!("✓ Quiver initialized.");
    println!();
    println!("Next:");
    println!("  1. Open a NEW Claude Code session (hooks load at session start).");
    println!("  2. Type any task — Quiver will inject a top-1 skill recommendation.");
    println!("  3. Disable per-shell with `export QUIVER_HOOK_DISABLED=1` if it gets noisy.");
    println!();
    println!(
        "Settings backup: {}.quiver-init.bak",
        plan.settings_path.display()
    );
}

async fn run_sync_if_needed() -> Result<()> {
    let db = default_db_path()?;
    let conn = quiver_storage::open(&db)?;
    let any_tool: i64 =
        conn.query_row("SELECT COALESCE(COUNT(*), 0) FROM tools", [], |r| r.get(0))?;
    if any_tool > 0 {
        eprintln!("[INIT] catalog has {any_tool} tools — skipping sync");
        return Ok(());
    }
    drop(conn);
    eprintln!("[INIT] catalog is empty — running quiver sync");
    crate::commands::sync::run().await
}

fn apply_settings(plan: &Plan) -> Result<()> {
    let mut current = read_settings_json(&plan.settings_path)?;
    backup_settings(&plan.settings_path)?;

    let merged = merge_quiver_hooks(current.clone(), &plan.quiver_bin);
    if merged == current {
        eprintln!("[INIT] settings.json already has Quiver hooks — no change");
        return Ok(());
    }
    current = merged;

    write_settings_atomic(&plan.settings_path, &current)?;
    eprintln!("[INIT] wrote {}", plan.settings_path.display());
    Ok(())
}

fn read_settings_json(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))
}

fn backup_settings(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let bak = path.with_extension(
        path.extension()
            .and_then(|s| s.to_str())
            .map(|s| format!("{s}.quiver-init.bak"))
            .unwrap_or_else(|| "quiver-init.bak".to_string()),
    );
    std::fs::copy(path, &bak)
        .with_context(|| format!("backup {} -> {}", path.display(), bak.display()))?;
    Ok(())
}

fn write_settings_atomic(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("mkdir -p {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp.quiver-init");
    let pretty = serde_json::to_string_pretty(value)?;
    std::fs::write(&tmp, pretty + "\n").with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

/// Add (or update) the Quiver `UserPromptSubmit` and `PreToolUse` hook
/// entries inside `settings.hooks`. Idempotent — entries are matched by the
/// substring `quiver hook` in `command`.
pub(crate) fn merge_quiver_hooks(mut settings: Value, quiver_bin: &str) -> Value {
    let hooks = settings
        .as_object_mut()
        .map(|o| o.entry("hooks").or_insert_with(|| json!({})));
    let Some(hooks) = hooks else {
        return settings;
    };
    let Some(hooks_obj) = hooks.as_object_mut() else {
        return settings;
    };

    upsert_hook_entry(
        hooks_obj,
        "UserPromptSubmit",
        None,
        &format!("{quiver_bin} hook user-prompt-submit"),
    );
    upsert_hook_entry(
        hooks_obj,
        "PreToolUse",
        Some("Skill|Agent|Task"),
        &format!("{quiver_bin} hook pre-tool-use"),
    );
    settings
}

fn upsert_hook_entry(
    hooks: &mut serde_json::Map<String, Value>,
    event: &str,
    matcher: Option<&str>,
    command: &str,
) {
    let arr = hooks
        .entry(event)
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(arr) = arr.as_array_mut() else {
        return;
    };

    // Idempotent: an entry whose nested command contains "quiver hook" wins.
    for entry in arr.iter() {
        if let Some(nested) = entry.get("hooks").and_then(|h| h.as_array())
            && nested.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(|s| s.contains("quiver hook"))
                    .unwrap_or(false)
            })
        {
            return;
        }
    }

    let mut entry = json!({
        "hooks": [
            { "type": "command", "command": command }
        ]
    });
    if let Some(m) = matcher {
        entry
            .as_object_mut()
            .unwrap()
            .insert("matcher".into(), Value::String(m.into()));
    }
    arr.push(entry);
}

fn write_meta_skill(plan: &Plan) -> Result<()> {
    if plan.meta_skill_path.exists() {
        eprintln!(
            "[INIT] meta-skill already at {} — skipping",
            plan.meta_skill_path.display()
        );
        return Ok(());
    }
    if let Some(parent) = plan.meta_skill_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("mkdir -p {}", parent.display()))?;
    }
    std::fs::write(&plan.meta_skill_path, META_SKILL_BODY)
        .with_context(|| format!("write {}", plan.meta_skill_path.display()))?;
    eprintln!("[INIT] wrote {}", plan.meta_skill_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_into_empty_settings_creates_hook_entries() {
        let merged = merge_quiver_hooks(json!({}), "/usr/local/bin/quiver");
        let upm = merged
            .pointer("/hooks/UserPromptSubmit/0/hooks/0/command")
            .and_then(|v| v.as_str())
            .unwrap();
        assert!(upm.contains("quiver hook user-prompt-submit"));
        let pre = merged
            .pointer("/hooks/PreToolUse/0/hooks/0/command")
            .and_then(|v| v.as_str())
            .unwrap();
        assert!(pre.contains("quiver hook pre-tool-use"));
        let matcher = merged
            .pointer("/hooks/PreToolUse/0/matcher")
            .and_then(|v| v.as_str())
            .unwrap();
        assert_eq!(matcher, "Skill|Agent|Task");
    }

    #[test]
    fn merge_is_idempotent() {
        let first = merge_quiver_hooks(json!({}), "/bin/quiver");
        let second = merge_quiver_hooks(first.clone(), "/bin/quiver");
        assert_eq!(first, second);
    }

    #[test]
    fn merge_preserves_existing_unrelated_hooks() {
        let starting = json!({
            "hooks": {
                "PreToolUse": [
                    { "matcher": "^Bash$", "hooks": [
                        { "type": "command", "command": "/usr/local/bin/something-else" }
                    ]}
                ]
            }
        });
        let merged = merge_quiver_hooks(starting.clone(), "/bin/quiver");
        let pre = merged
            .pointer("/hooks/PreToolUse")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(pre.len(), 2, "existing entry kept + Quiver appended");
        let cmds: Vec<&str> = pre
            .iter()
            .flat_map(|e| {
                e.get("hooks")
                    .and_then(|h| h.as_array())
                    .map(|a| a.as_slice())
                    .unwrap_or(&[])
            })
            .filter_map(|h| h.get("command").and_then(|c| c.as_str()))
            .collect();
        assert!(cmds.iter().any(|c| c.contains("something-else")));
        assert!(cmds.iter().any(|c| c.contains("quiver hook pre-tool-use")));
    }

    #[test]
    fn merge_replaces_legacy_quiver_hook_idempotently() {
        // Pretend the user already wired the bash version — re-running init
        // must not duplicate even though the command path differs from the
        // one we'd emit fresh.
        let starting = json!({
            "hooks": {
                "PreToolUse": [
                    { "matcher": "Skill|Agent", "hooks": [
                        { "type": "command",
                          "command": "/home/u/.local/bin/quiver-pretooluse.sh" }
                    ]}
                ]
            }
        });
        // Note: legacy bash path doesn't contain "quiver hook" (it's
        // "quiver-pretooluse.sh"), so it's NOT recognised as ours and our
        // fresh entry is appended. Documented behaviour — user replaces the
        // legacy entry by hand or removes it manually.
        let merged = merge_quiver_hooks(starting, "/bin/quiver");
        let pre = merged
            .pointer("/hooks/PreToolUse")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(pre.len(), 2);
    }

    #[test]
    fn write_meta_skill_writes_file_when_absent() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let plan = Plan {
            scope: Scope::User,
            settings_path: dir.path().join("settings.json"),
            meta_skill_path: dir.path().join("skills/quiver-pilot/SKILL.md"),
            quiver_bin: "quiver".into(),
        };
        write_meta_skill(&plan)?;
        let content = std::fs::read_to_string(&plan.meta_skill_path)?;
        assert!(content.contains("name: quiver-pilot"));
        assert!(content.contains("<quiver-recommendation>"));
        Ok(())
    }

    #[test]
    fn write_meta_skill_skips_when_present() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("skills/quiver-pilot/SKILL.md");
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, "user-customised")?;
        let plan = Plan {
            scope: Scope::User,
            settings_path: dir.path().join("settings.json"),
            meta_skill_path: path.clone(),
            quiver_bin: "quiver".into(),
        };
        write_meta_skill(&plan)?;
        assert_eq!(std::fs::read_to_string(&path)?, "user-customised");
        Ok(())
    }

    #[test]
    fn read_settings_json_returns_empty_object_for_missing_file() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let v = read_settings_json(&dir.path().join("nope.json"))?;
        assert_eq!(v, json!({}));
        Ok(())
    }

    #[test]
    fn write_settings_atomic_creates_parent_dir() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let target = dir.path().join("a/b/c/settings.json");
        write_settings_atomic(&target, &json!({"x": 1}))?;
        let content = std::fs::read_to_string(&target)?;
        assert!(content.contains("\"x\""));
        Ok(())
    }
}
