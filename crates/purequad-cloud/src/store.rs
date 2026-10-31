//! Where the apps keep the PureQuad Cloud sign-in and the working copies.
//!
//! The sign-in (server + session token) is one small file readable only by the user, shared by the
//! four apps, so signing in from one signs in all of them. The token is a revocable session: it
//! shows in the web app's list of sessions (named after the app) and ends with sign-out or wipe.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::Result;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Saved {
    pub server: String,
    pub token: String,
}

/// `~/Library/Application Support/PureQuad`, `%APPDATA%\PureQuad`, `$XDG_CONFIG_HOME/purequad`.
pub fn config_dir() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Application Support/PureQuad"))
    } else if cfg!(windows) {
        env("APPDATA").map(|a| a.join("PureQuad"))
    } else {
        env("XDG_CONFIG_HOME").or_else(|| env("HOME").map(|h| h.join(".config"))).map(|c| c.join("purequad"))
    }
}

/// Working copies of cloud files: `~/Library/Caches/PureQuad`, `%LOCALAPPDATA%\PureQuad\Cloud`,
/// `$XDG_CACHE_HOME/purequad`.
pub fn work_dir() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Caches/PureQuad"))
    } else if cfg!(windows) {
        env("LOCALAPPDATA").map(|a| a.join("PureQuad").join("Cloud"))
    } else {
        env("XDG_CACHE_HOME").or_else(|| env("HOME").map(|h| h.join(".cache"))).map(|c| c.join("purequad"))
    }
}

fn path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("cloud.json"))
}

pub fn load() -> Option<Saved> {
    let text = std::fs::read_to_string(path()?).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn save(saved: &Saved) -> Result<()> {
    let Some(p) = path() else { return Ok(()) };
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(saved).map_err(|e| crate::Error::Other(e.to_string()))?;
    write_private(&p, text.as_bytes())
}

/// Forget the sign-in (File › Sign out of PureQuad Cloud).
pub fn clear() {
    if let Some(p) = path() {
        let _ = std::fs::remove_file(p);
    }
}

/// Write a file only its owner can read (0600 on Unix; per-user `%APPDATA%` on Windows).
fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    #[cfg(not(unix))]
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
