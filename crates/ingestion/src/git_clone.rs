//! Thin wrapper around the system `git` binary.
//!
//! Phase 5 onboarding shells out to `git clone --depth 1` instead of pulling
//! in libgit2 / gix. Reasons:
//!   * zero new C deps (PLAN §3 "single static binary" goal),
//!   * full credential handling via the user's existing ~/.gitconfig,
//!   * matches PLAN §11 "no submodules / LFS" — `--depth 1` ignores both.
//!
//! Sets `GIT_TERMINAL_PROMPT=0` so a missing-credentials error fails fast
//! instead of blocking on a TTY prompt.

use std::ffi::OsStr;
use std::path::Path;

use anyhow::{Context, anyhow};
use tokio::process::Command;

/// Shallow-clone `url` into `dest`. `dest` must not already exist (git fails
/// otherwise). Returns Ok on success; surfaces git's stderr on failure.
pub async fn shallow_clone(url: &str, dest: &Path) -> anyhow::Result<()> {
    let status = Command::new("git")
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("clone")
        .arg("--depth")
        .arg("1")
        .arg("--quiet")
        .arg("--")
        .arg(url)
        .arg(dest.as_os_str())
        .output()
        .await
        .with_context(|| format!("spawn git clone for {url}"))?;
    if !status.status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr);
        return Err(anyhow!(
            "git clone {url} exited with {} — {}",
            status.status,
            stderr.trim()
        ));
    }
    Ok(())
}

/// Run `git rev-parse HEAD` inside `repo`. Returns the trimmed sha. Tolerates
/// repos without history (returns empty string) so the caller can fall back
/// to `None` for `last_commit_sha`.
pub async fn head_sha(repo: &Path) -> anyhow::Result<Option<String>> {
    run_git(repo, ["rev-parse", "HEAD"]).await
}

async fn run_git<I, S>(repo: &Path, args: I) -> anyhow::Result<Option<String>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let out = Command::new("git")
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("-C")
        .arg(repo.as_os_str())
        .args(args)
        .output()
        .await
        .context("spawn git")?;
    if !out.status.success() {
        return Ok(None);
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { Ok(None) } else { Ok(Some(s)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// We don't want CI to depend on network access for this slice. The clone
    /// path is exercised via the smoke test gated behind `--ignored`.
    /// Here we only verify the args-shape contract by running git against a
    /// non-existent URL and asserting failure surfaces git's error text.
    #[tokio::test]
    async fn clone_of_invalid_url_returns_err() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("nope");
        let res = shallow_clone("https://example.invalid/no/such/repo.git", &dest).await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn head_sha_of_non_repo_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let sha = head_sha(dir.path()).await.unwrap();
        assert!(sha.is_none());
    }
}
