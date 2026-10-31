//! Requests from the editors' own UI (title-bar cloud button, File menu) to the PureQuad Cloud
//! window, which lives in the app's glue (`cloud_links.rs`). The UI crates don't know the glue,
//! so they leave a request here and the glue picks it up on its next frame.

use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// Show the PureQuad Cloud window to open a file.
    Browse,
    /// Show it to save a copy of this local file to the cloud (then keep that copy in sync).
    SaveCopy(PathBuf),
    /// Save to the cloud, but the document has never been saved on this computer.
    SaveUnsaved,
}

static QUEUE: Mutex<Vec<Request>> = Mutex::new(Vec::new());

/// Ask for the PureQuad Cloud window (call `ctx.request_repaint()` after, if you have it).
pub fn request(r: Request) {
    if let Ok(mut q) = QUEUE.lock() {
        q.push(r);
    }
}

/// Ask to save `path` (or, for a document without a file yet, `None`) to the cloud.
pub fn save_copy(path: Option<PathBuf>) {
    request(path.map_or(Request::SaveUnsaved, Request::SaveCopy));
}

/// Everything asked for since the last call.
pub fn take() -> Vec<Request> {
    QUEUE.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default()
}
