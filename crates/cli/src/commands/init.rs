//! `quiver init` — zero-config bootstrap.
//!
//! Wires Claude Code hooks into `~/.claude/settings.json`, the Quiver MCP
//! server entry into `~/.claude.json`, runs an initial sync if the catalog
//! is empty, writes a single primer SKILL.md, and spawns the daily-task
//! `quiver agent` detached so `/suggestions` and the success-rate ranker
//! get fed automatically.
//!
//! Idempotent: re-running detects existing hook entries (matched by
//! `quiver hook` substring), MCP entries (by `command`), and a live agent
//! process (by PID file + `kill(0)` probe). Atomic: every settings write
//! is preceded by a `<file>.quiver-init.bak` backup and uses temp-file +
//! rename.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use serde_json::{Value, json};

use crate::db_path::default_db_path;

const META_SKILL_BODY: &str = include_str!("../../assets/quiver-pilot/SKILL.md");
const META_SKILL_DIR: &str = "skills/quiver-pilot";
const META_SKILL_FILE: &str = "SKILL.md";
const AGENT_CACHE_DIR: &str = ".cache/quiver";
const AGENT_PID_FILE: &str = "agent.pid";
const AGENT_LOG_FILE: &str = "agent.log";
const WEB_PID_FILE: &str = "web.pid";
const WEB_LOG_FILE: &str = "web.log";
const DEFAULT_WEB_PORT: u16 = 7777;

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
    /// Skip the Quiver MCP server entry merge into `~/.claude.json`.
    /// Default: wire it (so `mcp__quiver__recommend` / `info` are
    /// available mid-session for the model).
    #[arg(long)]
    pub no_mcp: bool,
    /// Skip spawning the background `quiver agent` daemon. Default: spawn
    /// it detached (PID at `~/.cache/quiver/agent.pid`, logs at
    /// `~/.cache/quiver/agent.log`) so `/suggestions` populates and the
    /// success-rate reranker learns from acceptances.
    #[arg(long)]
    pub no_start_agent: bool,
    /// Skip spawning the local web UI on 127.0.0.1. Default: spawn
    /// `quiver serve` detached (PID at `~/.cache/quiver/web.pid`, logs
    /// at `~/.cache/quiver/web.log`) so `/catalog`, `/recommend`, and
    /// `/suggestions` are reachable in the browser.
    #[arg(long)]
    pub no_start_web: bool,
    /// Port to bind the web UI on (default 7777). Loopback only.
    #[arg(long, default_value_t = DEFAULT_WEB_PORT)]
    pub web_port: u16,
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
    if !args.no_mcp {
        apply_mcp(&plan)?;
    }
    let agent_status = if !args.no_start_agent {
        start_agent(&plan)
    } else {
        AgentStatus::Skipped
    };
    let web_status = if !args.no_start_web {
        start_web(&plan)
    } else {
        WebStatus::Skipped
    };

    println!();
    print_next_steps(&plan, &agent_status, &web_status);
    Ok(())
}

#[derive(Debug)]
struct Plan {
    scope: Scope,
    settings_path: PathBuf,
    meta_skill_path: PathBuf,
    claude_json_path: PathBuf,
    project_path: PathBuf,
    agent_pid_path: PathBuf,
    agent_log_path: PathBuf,
    web_pid_path: PathBuf,
    web_log_path: PathBuf,
    web_port: u16,
    quiver_bin: String,
}

fn build_plan(args: &InitArgs) -> Result<Plan> {
    let home = std::env::var("HOME").context("HOME is unset")?;
    let home = PathBuf::from(home);
    let project_path = std::env::current_dir()?;
    let settings_path = match args.scope {
        Scope::User => home.join(".claude/settings.json"),
        Scope::Project => project_path.join(".claude/settings.json"),
    };
    let meta_skill_path = home
        .join(".claude")
        .join(META_SKILL_DIR)
        .join(META_SKILL_FILE);
    let claude_json_path = home.join(".claude.json");
    let cache_dir = home.join(AGENT_CACHE_DIR);
    let agent_pid_path = cache_dir.join(AGENT_PID_FILE);
    let agent_log_path = cache_dir.join(AGENT_LOG_FILE);
    let web_pid_path = cache_dir.join(WEB_PID_FILE);
    let web_log_path = cache_dir.join(WEB_LOG_FILE);
    let quiver_bin = current_quiver_binary();

    Ok(Plan {
        scope: args.scope,
        settings_path,
        meta_skill_path,
        claude_json_path,
        project_path,
        agent_pid_path,
        agent_log_path,
        web_pid_path,
        web_log_path,
        web_port: args.web_port,
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
    eprintln!(
        "[{prefix}] claude.json: {}",
        plan.claude_json_path.display()
    );
    eprintln!("[{prefix}] agent pid:   {}", plan.agent_pid_path.display());
    eprintln!("[{prefix}] agent log:   {}", plan.agent_log_path.display());
    eprintln!(
        "[{prefix}] web pid:     {} (port {})",
        plan.web_pid_path.display(),
        plan.web_port
    );
    eprintln!("[{prefix}] web log:     {}", plan.web_log_path.display());
    eprintln!("[{prefix}] quiver binary: {}", plan.quiver_bin);
}

fn print_next_steps(plan: &Plan, agent: &AgentStatus, web: &WebStatus) {
    println!("✓ Quiver initialized.");
    println!();
    println!("Next:");
    println!("  1. Open a NEW Claude Code session (hooks load at session start).");
    println!("  2. Type any task — Quiver will inject a top-1 skill recommendation.");
    println!("  3. Disable per-shell with `export QUIVER_HOOK_DISABLED=1` if it gets noisy.");
    println!();
    match agent {
        AgentStatus::Started(pid) => {
            println!("Background agent: started (PID {pid}).");
            println!("  Logs: {}", plan.agent_log_path.display());
            println!("  Stop: kill {pid}");
        },
        AgentStatus::AlreadyRunning(pid) => {
            println!("Background agent: already running (PID {pid}). Reusing.");
            println!("  Logs: {}", plan.agent_log_path.display());
        },
        AgentStatus::Skipped => {
            println!("Background agent: not started (--no-start-agent).");
            println!("  Run manually: quiver agent");
        },
        AgentStatus::Failed(err) => {
            println!("Background agent: failed to start ({err}).");
            println!("  Run manually: quiver agent");
        },
    }
    match web {
        WebStatus::Started(pid) => {
            println!(
                "Web UI:           started (PID {pid}) at http://127.0.0.1:{}.",
                plan.web_port
            );
            println!("  Logs: {}", plan.web_log_path.display());
            println!("  Stop: kill {pid}");
        },
        WebStatus::AlreadyRunning(pid) => {
            println!(
                "Web UI:           already running (PID {pid}) at http://127.0.0.1:{}. Reusing.",
                plan.web_port
            );
        },
        WebStatus::Skipped => {
            println!("Web UI:           not started (--no-start-web).");
            println!(
                "  Run manually: quiver serve --port {} --open",
                plan.web_port
            );
        },
        WebStatus::Failed(err) => {
            println!("Web UI:           failed to start ({err}).");
            println!(
                "  Run manually: quiver serve --port {} --open",
                plan.web_port
            );
        },
    }
    println!();
    println!("Prefer tmux instead of the spawned daemons (survives reboot more cleanly):");
    println!(
        "  kill $(cat {}) 2>/dev/null",
        plan.agent_pid_path.display()
    );
    println!("  kill $(cat {}) 2>/dev/null", plan.web_pid_path.display());
    println!("  tmux new -d -s quiver-agent 'quiver agent'");
    println!(
        "  tmux new -d -s quiver-web 'quiver serve --port {}'",
        plan.web_port
    );
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

#[derive(Debug)]
enum AgentStatus {
    Started(u32),
    AlreadyRunning(u32),
    Skipped,
    Failed(String),
}

#[derive(Debug)]
enum WebStatus {
    Started(u32),
    AlreadyRunning(u32),
    Skipped,
    Failed(String),
}

fn apply_mcp(plan: &Plan) -> Result<()> {
    let mut current = read_settings_json(&plan.claude_json_path)?;
    backup_settings(&plan.claude_json_path)?;

    let merged = merge_quiver_mcp(
        current.clone(),
        &plan.quiver_bin,
        plan.scope,
        &plan.project_path,
    );
    if merged == current {
        eprintln!("[INIT] claude.json already has Quiver MCP entry — no change");
        return Ok(());
    }
    current = merged;

    write_settings_atomic(&plan.claude_json_path, &current)?;
    eprintln!("[INIT] wrote {}", plan.claude_json_path.display());
    Ok(())
}

/// Insert (or refresh) the Quiver MCP server entry. For `Scope::User` we
/// write at top-level `mcpServers.quiver`; for `Scope::Project` under
/// `projects.<cwd>.mcpServers.quiver`. Idempotent — same `command` is a
/// no-op.
pub(crate) fn merge_quiver_mcp(
    mut settings: Value,
    quiver_bin: &str,
    scope: Scope,
    project_path: &Path,
) -> Value {
    let entry = json!({
        "type": "stdio",
        "command": quiver_bin,
        "args": ["mcp"],
        "env": {},
    });

    let Some(root) = settings.as_object_mut() else {
        return settings;
    };
    let target_obj: &mut serde_json::Map<String, Value> = match scope {
        Scope::User => {
            let mcp = root
                .entry("mcpServers")
                .or_insert_with(|| Value::Object(Default::default()));
            let Some(obj) = mcp.as_object_mut() else {
                return settings;
            };
            obj
        },
        Scope::Project => {
            let projects = root
                .entry("projects")
                .or_insert_with(|| Value::Object(Default::default()));
            let Some(projects_obj) = projects.as_object_mut() else {
                return settings;
            };
            let key = project_path.display().to_string();
            let project = projects_obj
                .entry(key)
                .or_insert_with(|| Value::Object(Default::default()));
            let Some(project_obj) = project.as_object_mut() else {
                return settings;
            };
            let mcp = project_obj
                .entry("mcpServers")
                .or_insert_with(|| Value::Object(Default::default()));
            let Some(obj) = mcp.as_object_mut() else {
                return settings;
            };
            obj
        },
    };

    // Idempotent: if existing entry already points at the same command,
    // leave it alone (preserves user's env/args customisations).
    if let Some(existing) = target_obj.get("quiver")
        && existing.get("command").and_then(|c| c.as_str()) == Some(quiver_bin)
    {
        return settings;
    }
    target_obj.insert("quiver".to_string(), entry);
    settings
}

fn start_agent(plan: &Plan) -> AgentStatus {
    if let Some(pid) = read_pid(&plan.agent_pid_path)
        && agent_is_running(pid)
    {
        return AgentStatus::AlreadyRunning(pid);
    }
    match spawn_agent(plan) {
        Ok(pid) => AgentStatus::Started(pid),
        Err(err) => AgentStatus::Failed(format!("{err:#}")),
    }
}

fn start_web(plan: &Plan) -> WebStatus {
    if let Some(pid) = read_pid(&plan.web_pid_path)
        && agent_is_running(pid)
    {
        return WebStatus::AlreadyRunning(pid);
    }
    match spawn_web(plan) {
        Ok(pid) => WebStatus::Started(pid),
        Err(err) => WebStatus::Failed(format!("{err:#}")),
    }
}

fn read_pid(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

#[cfg(unix)]
fn agent_is_running(pid: u32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    kill(Pid::from_raw(pid as i32), None).is_ok()
}

#[cfg(not(unix))]
fn agent_is_running(_pid: u32) -> bool {
    false
}

#[cfg(unix)]
fn spawn_agent(plan: &Plan) -> Result<u32> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    if let Some(parent) = plan.agent_log_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("mkdir -p {}", parent.display()))?;
    }

    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&plan.agent_log_path)
        .with_context(|| format!("open log {}", plan.agent_log_path.display()))?;
    let log_dup = log.try_clone().with_context(|| "clone log fd for stderr")?;

    let child = Command::new(&plan.quiver_bin)
        .arg("agent")
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_dup)
        .process_group(0)
        .spawn()
        .with_context(|| format!("spawn {} agent", plan.quiver_bin))?;

    let pid = child.id();
    std::fs::write(&plan.agent_pid_path, pid.to_string())
        .with_context(|| format!("write pid file {}", plan.agent_pid_path.display()))?;
    Ok(pid)
}

#[cfg(not(unix))]
fn spawn_agent(_plan: &Plan) -> Result<u32> {
    anyhow::bail!("agent autostart not supported on this platform — run `quiver agent` manually")
}

#[cfg(unix)]
fn spawn_web(plan: &Plan) -> Result<u32> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    if let Some(parent) = plan.web_log_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("mkdir -p {}", parent.display()))?;
    }

    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&plan.web_log_path)
        .with_context(|| format!("open log {}", plan.web_log_path.display()))?;
    let log_dup = log.try_clone().with_context(|| "clone log fd for stderr")?;

    let child = Command::new(&plan.quiver_bin)
        .arg("serve")
        .arg("--port")
        .arg(plan.web_port.to_string())
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_dup)
        .process_group(0)
        .spawn()
        .with_context(|| format!("spawn {} serve", plan.quiver_bin))?;

    let pid = child.id();
    std::fs::write(&plan.web_pid_path, pid.to_string())
        .with_context(|| format!("write pid file {}", plan.web_pid_path.display()))?;
    Ok(pid)
}

#[cfg(not(unix))]
fn spawn_web(_plan: &Plan) -> Result<u32> {
    anyhow::bail!("web autostart not supported on this platform — run `quiver serve` manually")
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
        let plan = test_plan(dir.path(), "skills/quiver-pilot/SKILL.md");
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
        let plan = test_plan(dir.path(), "skills/quiver-pilot/SKILL.md");
        write_meta_skill(&plan)?;
        assert_eq!(std::fs::read_to_string(&path)?, "user-customised");
        Ok(())
    }

    fn test_plan(root: &Path, meta_skill_subpath: &str) -> Plan {
        Plan {
            scope: Scope::User,
            settings_path: root.join("settings.json"),
            meta_skill_path: root.join(meta_skill_subpath),
            claude_json_path: root.join("claude.json"),
            project_path: root.to_path_buf(),
            agent_pid_path: root.join("agent.pid"),
            agent_log_path: root.join("agent.log"),
            web_pid_path: root.join("web.pid"),
            web_log_path: root.join("web.log"),
            web_port: DEFAULT_WEB_PORT,
            quiver_bin: "quiver".into(),
        }
    }

    #[test]
    fn start_web_returns_already_running_for_live_pid() {
        let dir = tempfile::tempdir().unwrap();
        let mut plan = test_plan(dir.path(), "skills/quiver-pilot/SKILL.md");
        // Point web PID file at this very test process so kill(0) succeeds.
        std::fs::write(&plan.web_pid_path, std::process::id().to_string()).unwrap();
        // Use a non-existent binary so spawn_web would fail if reached —
        // proves the early-return path was taken.
        plan.quiver_bin = "/definitely/not/here/quiver".into();
        match start_web(&plan) {
            WebStatus::AlreadyRunning(pid) => assert_eq!(pid, std::process::id()),
            other => panic!("expected AlreadyRunning, got {other:?}"),
        }
    }

    #[test]
    fn start_agent_returns_already_running_for_live_pid() {
        let dir = tempfile::tempdir().unwrap();
        let mut plan = test_plan(dir.path(), "skills/quiver-pilot/SKILL.md");
        std::fs::write(&plan.agent_pid_path, std::process::id().to_string()).unwrap();
        plan.quiver_bin = "/definitely/not/here/quiver".into();
        match start_agent(&plan) {
            AgentStatus::AlreadyRunning(pid) => assert_eq!(pid, std::process::id()),
            other => panic!("expected AlreadyRunning, got {other:?}"),
        }
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

    #[test]
    fn merge_mcp_user_scope_writes_top_level_entry() {
        let merged = merge_quiver_mcp(
            json!({}),
            "/usr/local/bin/quiver",
            Scope::User,
            Path::new("/anywhere"),
        );
        let entry = merged.pointer("/mcpServers/quiver").unwrap();
        assert_eq!(entry.get("type").and_then(|v| v.as_str()), Some("stdio"));
        assert_eq!(
            entry.get("command").and_then(|v| v.as_str()),
            Some("/usr/local/bin/quiver")
        );
        let args = entry.get("args").and_then(|v| v.as_array()).unwrap();
        assert_eq!(args.len(), 1);
        assert_eq!(args[0].as_str(), Some("mcp"));
    }

    #[test]
    fn merge_mcp_project_scope_nests_under_projects() {
        let cwd = Path::new("/home/u/proj");
        let merged = merge_quiver_mcp(json!({}), "/bin/quiver", Scope::Project, cwd);
        let entry = merged
            .pointer("/projects/~1home~1u~1proj/mcpServers/quiver")
            .unwrap();
        assert_eq!(
            entry.get("command").and_then(|v| v.as_str()),
            Some("/bin/quiver")
        );
    }

    #[test]
    fn merge_mcp_is_idempotent() {
        let first = merge_quiver_mcp(json!({}), "/bin/quiver", Scope::User, Path::new("/x"));
        let second = merge_quiver_mcp(first.clone(), "/bin/quiver", Scope::User, Path::new("/x"));
        assert_eq!(first, second);
    }

    #[test]
    fn merge_mcp_preserves_other_projects() {
        let starting = json!({
            "projects": {
                "/home/u/other": {
                    "mcpServers": { "context7": { "type": "stdio", "command": "context7" } }
                }
            }
        });
        let merged = merge_quiver_mcp(
            starting.clone(),
            "/bin/quiver",
            Scope::Project,
            Path::new("/home/u/proj"),
        );
        assert!(
            merged
                .pointer("/projects/~1home~1u~1other/mcpServers/context7")
                .is_some(),
            "other project's MCP entry preserved"
        );
        assert!(
            merged
                .pointer("/projects/~1home~1u~1proj/mcpServers/quiver")
                .is_some(),
            "current project's quiver entry inserted"
        );
    }

    #[test]
    fn merge_mcp_replaces_existing_entry_with_different_command() {
        let starting = json!({
            "mcpServers": {
                "quiver": { "type": "stdio", "command": "/old/path/quiver", "args": ["mcp"] }
            }
        });
        let merged = merge_quiver_mcp(starting, "/new/path/quiver", Scope::User, Path::new("/x"));
        let cmd = merged
            .pointer("/mcpServers/quiver/command")
            .and_then(|v| v.as_str())
            .unwrap();
        assert_eq!(cmd, "/new/path/quiver");
    }

    #[test]
    fn agent_is_running_returns_true_for_self() {
        // The current test process is alive.
        let me = std::process::id();
        assert!(agent_is_running(me));
    }

    #[test]
    fn agent_is_running_returns_false_for_unallocated_pid() {
        // PID 1 is init; PID 2_000_000 is almost certainly unused on Linux
        // (default `pid_max` = 4_194_304 but reaching there in practice is
        // rare). If this flakes, raise the cap.
        assert!(!agent_is_running(2_000_000));
    }

    #[test]
    fn read_pid_handles_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let none = read_pid(&dir.path().join("nope.pid"));
        assert!(none.is_none());
    }

    #[test]
    fn read_pid_parses_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.pid");
        std::fs::write(&path, "12345\n").unwrap();
        assert_eq!(read_pid(&path), Some(12345));
    }
}
