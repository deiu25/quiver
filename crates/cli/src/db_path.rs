use std::path::PathBuf;

use anyhow::anyhow;

/// Resolve the default Quiver SQLite path:
/// `$XDG_DATA_HOME/quiver/quiver.sqlite`, falling back to
/// `$HOME/.local/share/quiver/quiver.sqlite`.
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
    Ok(base.join("quiver").join("quiver.sqlite"))
}
