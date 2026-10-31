//! End-to-end check of the browser sign-in against a running cloud-rs:
//!   cargo run --example signin_e2e -- http://127.0.0.1:8080
//! Prints the sign-in page; the caller plays the browser. Then lists the root and saves a file.
use std::time::Duration;

fn main() {
    let server = std::env::args().nth(1).expect("server");
    let s = purequad_cloud::signin::start(&server, "PureQuad Sheets", |url| println!("URL {url}"), || {}).expect("start");
    let client = loop {
        if let Some(r) = s.poll() {
            break r.expect("signed in");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let me = client.me().expect("me");
    println!("signed in as {}", me.email);
    let saved = purequad_cloud::signin::saved_client().expect("saved");
    assert_eq!(saved.token(), client.token());
    let node = client.upload_new(None, "Buget-desktop.xlsx", b"sheet bytes").expect("upload_new");
    let root = client.list(None).expect("list");
    assert!(root.iter().any(|n| n.id == node.id));
    println!("shared with me: {}", client.shared_with_me().expect("shared").len());
    println!("SIGNIN E2E OK");
}
