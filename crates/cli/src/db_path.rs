use std::path::PathBuf;

use anyhow::anyhow;

/// Resolve the default ToolHub SQLite path:
/// `$XDG_DATA_HOME/toolhub/toolhub.sqlite`, falling back to
/// `$HOME/.local/share/toolhub/toolhub.sqlite`.
pub fn default_db_path() -> anyhow::Result<PathBuf> {
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .ok()
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".local/share"))
        })
        .ok_or_else(|| anyhow!("cannot resolve XDG_DATA_HOME or HOME"))?;
    Ok(base.join("toolhub").join("toolhub.sqlite"))
}
