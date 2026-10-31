//! A cloud file's working copy: downloaded into a private folder, opened by the editor like any
//! local file, and uploaded as a new version of the cloud file each time the editor saves it.
//!
//! Saves are noticed by watching the file (size and modification time), so this works with every
//! editor and every way it saves (in place or write-and-rename) without touching its code.
//!
//! If someone else saved a newer version in the cloud meanwhile, nothing is overwritten: the save
//! goes up as a new file next to the original, "<name> (conflict <date>).<ext>", and the working
//! copy follows that file from then on.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::api::{Client, Node};
use crate::{Error, Result, store};

/// What the sync thread reports (for the editor's status bar).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Downloaded and open; saves will go to the cloud.
    Ready { name: String },
    /// Shared read-only: saves stay on this computer.
    ReadOnly { name: String },
    Uploading { name: String },
    /// A save is now the cloud file's newest version.
    Saved { name: String },
    /// Someone else saved first; this save became a new file.
    Conflict { name: String, copy: String },
    /// The upload failed; it is retried.
    Failed { message: String },
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ready { name } => write!(f, "{name} is open from PureQuad Cloud; saving updates it there"),
            Self::ReadOnly { name } => write!(f, "{name} is shared with you read-only; saves stay on this computer"),
            Self::Uploading { name } => write!(f, "Saving {name} to PureQuad Cloud…"),
            Self::Saved { name } => write!(f, "{name} saved to PureQuad Cloud"),
            Self::Conflict { name, copy } => {
                write!(f, "{name} changed in PureQuad Cloud meanwhile; your version was saved as {copy}")
            }
            Self::Failed { message } => write!(f, "Not saved to PureQuad Cloud yet: {message}"),
        }
    }
}

/// Kept next to the working copy (`.purequad.json`).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct Meta {
    server: String,
    file_id: String,
    parent_id: Option<String>,
    name: String,
    /// The cloud version the working copy is based on (`updated_at`).
    cloud_updated_at: String,
    /// The local file as it was last downloaded or uploaded: (length, modified in ms).
    synced: (u64, u128),
    writable: bool,
}

/// Keeps one working copy in sync. Dropping it stops the sync thread (after an upload under way).
pub struct WorkCopy {
    path: PathBuf,
    name: String,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl WorkCopy {
    /// Download cloud file `file_id` (unless the working copy has a save not uploaded yet) and
    /// start watching it.
    pub fn open(client: Client, file_id: &str, on_status: Box<dyn Fn(Status) + Send>) -> Result<Self> {
        let node = client.node(file_id)?;
        if node.is_folder() {
            return Err(Error::Other(format!("{} is a folder", node.name)));
        }
        let root = store::work_dir().ok_or_else(|| Error::Other("no folder for working copies".into()))?;
        let dir = root.join(&node.id);
        std::fs::create_dir_all(&dir)?;
        let name = safe_name(&node.name);
        let path = dir.join(&name);
        let meta_path = dir.join(".purequad.json");
        let old: Option<Meta> = std::fs::read_to_string(&meta_path).ok().and_then(|t| serde_json::from_str(&t).ok());
        // A save that never reached the cloud (offline, app closed mid-upload) is kept and sent now.
        let pending = old.as_ref().is_some_and(|m| m.name == name && stamp(&path).is_some_and(|s| s != m.synced));
        let meta = if pending {
            let mut m = old.unwrap_or_default();
            m.writable = node.writable();
            m
        } else {
            let bytes = client.download(&node.id)?;
            write_atomic(&path, &bytes)?;
            Meta {
                server: client.server().to_string(),
                file_id: node.id.clone(),
                parent_id: node.parent_id.clone(),
                name: name.clone(),
                cloud_updated_at: node.updated_at.clone(),
                synced: stamp(&path).unwrap_or_default(),
                writable: node.writable(),
            }
        };
        save_meta(&meta_path, &meta)?;
        on_status(if meta.writable { Status::Ready { name: node.name.clone() } } else { Status::ReadOnly { name: node.name.clone() } });
        let stop = Arc::new(AtomicBool::new(false));
        let thread = meta.writable.then(|| {
            let (stop, path) = (stop.clone(), path.clone());
            std::thread::Builder::new()
                .name("purequad-cloud-sync".into())
                .spawn(move || watch(client, path, meta_path, meta, stop, on_status))
                .ok()
        });
        Ok(Self { path, name: node.name, stop, thread: thread.flatten() })
    }

    /// The local file the editor opens.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The cloud file's name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for WorkCopy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

const POLL: Duration = Duration::from_millis(800);

fn watch(client: Client, path: PathBuf, meta_path: PathBuf, mut meta: Meta, stop: Arc<AtomicBool>, on_status: Box<dyn Fn(Status) + Send>) {
    let mut last_seen = None;
    let mut retry_at: Option<SystemTime> = None;
    let mut failures = 0u32;
    loop {
        // One last look after stop, so a save made just before quitting still goes up.
        let stopping = stop.load(Ordering::Relaxed);
        let now = stamp(&path);
        // Upload once the file has stopped changing for one poll (editors write in several steps).
        let settled = now.is_some() && now == last_seen;
        last_seen = now;
        let due = retry_at.is_none_or(|t| SystemTime::now() >= t) || stopping;
        if let Some(now) = now.filter(|n| *n != meta.synced && (settled || stopping) && due) {
            match upload(&client, &path, &mut meta, &on_status) {
                Ok(()) => {
                    meta.synced = now;
                    let _ = save_meta(&meta_path, &meta);
                    failures = 0;
                    retry_at = None;
                }
                Err(e) => {
                    log::warn!("PureQuad Cloud upload of {}: {e}", meta.name);
                    on_status(Status::Failed { message: e.to_string() });
                    failures += 1;
                    retry_at = Some(SystemTime::now() + Duration::from_secs(5u64.saturating_mul(1 << failures.min(6))));
                    if matches!(e, Error::SignedOut) {
                        return;
                    }
                }
            }
        }
        if stopping {
            return;
        }
        std::thread::sleep(POLL);
    }
}

fn upload(client: &Client, path: &Path, meta: &mut Meta, on_status: &dyn Fn(Status)) -> Result<()> {
    let bytes = std::fs::read(path)?;
    on_status(Status::Uploading { name: meta.name.clone() });
    let cloud = client.node(&meta.file_id);
    let unchanged = cloud.as_ref().is_ok_and(|n| n.updated_at == meta.cloud_updated_at);
    let node: Node = if unchanged {
        let node = client.upload_version(&meta.file_id, &meta.name, &bytes)?;
        on_status(Status::Saved { name: node.name.clone() });
        node
    } else {
        // Changed (or deleted) in the cloud: never overwrite someone else's version.
        if let Err(e) = cloud
            && !matches!(e, Error::Http { status: 404, .. })
        {
            return Err(e);
        }
        let copy = conflict_name(&meta.name, SystemTime::now());
        let node = client.upload_new(meta.parent_id.as_deref(), &copy, &bytes)?;
        on_status(Status::Conflict { name: meta.name.clone(), copy: node.name.clone() });
        meta.file_id = node.id.clone();
        node
    };
    meta.cloud_updated_at = node.updated_at;
    Ok(())
}

/// (length, modified time in ms), or `None` while the file is missing (mid-rename).
fn stamp(path: &Path) -> Option<(u64, u128)> {
    let m = std::fs::metadata(path).ok()?;
    let modified = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_millis();
    Some((m.len(), modified))
}

fn save_meta(path: &Path, meta: &Meta) -> Result<()> {
    let text = serde_json::to_string_pretty(meta).map_err(|e| Error::Other(e.to_string()))?;
    write_atomic(path, text.as_bytes())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_file_name(format!(".{}.download", path.file_name().and_then(|n| n.to_str()).unwrap_or("file")));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// A cloud name as a safe local file name: no path separators, no reserved characters.
/// Whether `path` is one of the working copies (so it is in the cloud already and every save
/// goes there by itself).
pub fn is_work_copy(path: &Path) -> bool {
    store::work_dir().is_some_and(|root| path.starts_with(root))
}

fn safe_name(name: &str) -> String {
    let cleaned: String =
        name.chars().map(|c| if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c }).collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim_end_matches(['.', ' ']).to_string();
    if cleaned.is_empty() { "document".into() } else { cleaned }
}

/// "Raport.docx" → "Raport (conflict 2026-10-31 1642).docx" (UTC).
fn conflict_name(name: &str, at: SystemTime) -> String {
    let secs = at.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default();
    let (y, mo, d) = civil(i64::try_from(secs / 86_400).unwrap_or_default());
    let (h, mi) = ((secs % 86_400) / 3600, (secs % 3600) / 60);
    let stamp = format!("conflict {y:04}-{mo:02}-{d:02} {h:02}{mi:02}");
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} ({stamp}).{ext}"),
        _ => format!("{name} ({stamp})"),
    }
}

/// Days since 1970-01-01 → (year, month, day), proleptic Gregorian (H. Hinnant's algorithm).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_safe_on_every_system() {
        assert_eq!(safe_name("Raport Q3.docx"), "Raport Q3.docx");
        assert_eq!(safe_name("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(safe_name("a:b*c?.xlsx"), "a_b_c_.xlsx");
        assert_eq!(safe_name(" . "), "document");
    }

    #[test]
    fn conflict_copies_keep_the_extension() {
        let at = UNIX_EPOCH + Duration::from_secs(1_793_465_000); // 2026-10-31 16:43 UTC
        assert_eq!(conflict_name("Raport.docx", at), "Raport (conflict 2026-10-31 1643).docx");
        assert_eq!(conflict_name("README", at), "README (conflict 2026-10-31 1643)");
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(19_782), (2024, 2, 29));
    }
}
