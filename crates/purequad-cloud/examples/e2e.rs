//! End-to-end check against a running cloud-rs:
//!   cargo run --example e2e -- '<purequad-docs://open?...link with a fresh ticket>'
//! Opens the link, appends to the working copy twice (two saves), then prints the statuses.
use std::sync::mpsc::channel;
use std::time::Duration;

fn main() {
    let link = std::env::args().nth(1).expect("link");
    let (tx, rx) = channel();
    let copy = purequad_cloud::open_link(&link, "PureQuad Docs", move |s| {
        let _ = tx.send(s);
    })
    .expect("open_link");
    println!("working copy: {}", copy.path().display());
    let wait = |want: &str| loop {
        let s = rx.recv_timeout(Duration::from_secs(30)).expect("status in time");
        println!("status: {s}");
        if format!("{s:?}").starts_with(want) {
            break s;
        }
    };
    wait("Ready");
    for i in 1..=2 {
        std::thread::sleep(Duration::from_millis(1100));
        let mut bytes = std::fs::read(copy.path()).unwrap();
        bytes.extend_from_slice(format!("\nedit {i}").as_bytes());
        std::fs::write(copy.path(), bytes).unwrap();
        wait("Saved");
    }
    if let Ok(path) = std::env::var("E2E_CONFLICT") {
        // The caller uploaded a version from "someone else" when this file appears.
        while !std::path::Path::new(&path).exists() {
            std::thread::sleep(Duration::from_millis(200));
        }
        let mut bytes = std::fs::read(copy.path()).unwrap();
        bytes.extend_from_slice(b"\nmy edit after theirs");
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(copy.path(), bytes).unwrap();
        wait("Conflict");
    }
    drop(copy);
    println!("E2E OK");
}
