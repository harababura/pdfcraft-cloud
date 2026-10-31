//! Sign in from the desktop app through the browser (RFC 8252, loopback redirect).
//!
//! The app listens on a random port of 127.0.0.1 and opens
//! `<server>/desktop-login?app=<name>&port=<port>&state=<random>` in the browser. There the user
//! is (or gets) signed in to the web app, with whatever it asks for (password, TOTP, passkey,
//! SSO), and confirms; the page sends the browser to `http://127.0.0.1:<port>/callback` with a
//! one-time ticket and the same `state`, which the app trades for its own session. The app never
//! sees a password.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use crate::api::Client;
use crate::{Error, Result, store};

/// How long the app waits for the browser.
const TIMEOUT: Duration = Duration::from_secs(300);

/// A sign-in under way. Poll [`SignIn::poll`]; dropping it gives up.
pub struct SignIn {
    /// The page to open in the browser (already opened by [`start`] when `open_browser` is set).
    pub url: String,
    rx: Receiver<Result<Client>>,
    cancel: Arc<AtomicBool>,
}

impl SignIn {
    /// `Some` once the browser came back (or the wait ended).
    pub fn poll(&self) -> Option<Result<Client>> {
        self.rx.try_recv().ok()
    }
}

impl Drop for SignIn {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Start signing in to `server`. `open` opens a URL in the system browser (the app's own
/// `open_url`); `wake` is called when the result is ready (`egui::Context::request_repaint`).
pub fn start(server: &str, app_name: &str, open: impl FnOnce(&str), wake: impl Fn() + Send + 'static) -> Result<SignIn> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let state = random_state();
    let server = server.trim_end_matches('/').to_string();
    let q = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("app", app_name)
        .append_pair("port", &port.to_string())
        .append_pair("state", &state)
        .finish();
    let url = format!("{server}/desktop-login?{q}");
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let (stop, client_name) = (cancel.clone(), format!("{app_name} (desktop)"));
    std::thread::Builder::new()
        .name("purequad-cloud-signin".into())
        .spawn(move || {
            let result = wait_for_ticket(&listener, &state, &stop).and_then(|ticket| {
                let client = Client::exchange_ticket(&server, &ticket, &client_name)?;
                store::save(&store::Saved { server: server.clone(), token: client.token().to_string() })?;
                Ok(client)
            });
            if !stop.load(Ordering::Relaxed) {
                let _ = tx.send(result);
                wake();
            }
        })
        .map_err(Error::Io)?;
    open(&url);
    Ok(SignIn { url, rx, cancel })
}

/// The saved sign-in, if any (checked against the server only when used).
pub fn saved_client() -> Option<Client> {
    store::load().map(|s| Client::new(&s.server, &s.token))
}

fn wait_for_ticket(listener: &TcpListener, state: &str, stop: &AtomicBool) -> Result<String> {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        if stop.load(Ordering::Relaxed) {
            return Err(Error::Other("sign-in cancelled".into()));
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if let Some(ticket) = handle(stream, state) {
                    return Ok(ticket);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(Error::Io(e)),
        }
    }
    Err(Error::Other("the browser didn't confirm the sign-in in time".into()))
}

/// Answer one browser request. Returns the ticket when it is the callback with our `state`.
fn handle(stream: TcpStream, state: &str) -> Option<String> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.by_ref().take(8192).read_line(&mut line).ok()?;
    let target = line.split_whitespace().nth(1)?.to_string();
    let url = url::Url::parse(&format!("http://127.0.0.1{target}")).ok()?;
    let mut stream = reader.into_inner();
    if url.path() != "/callback" {
        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return None;
    }
    let get = |k: &str| url.query_pairs().find(|(key, _)| key == k).map(|(_, v)| v.into_owned());
    let ok = get("state").as_deref() == Some(state);
    let ticket = get("ticket").filter(|t| ok && !t.is_empty());
    let (title, text) = if ticket.is_some() {
        ("Conectat", "Aplicația PureQuad este conectată la PureQuad Cloud. Poți închide această filă.")
    } else {
        ("Conectare eșuată", "Conectarea nu a putut fi confirmată. Încearcă din nou din aplicație.")
    };
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>{title}</title><body style=\"font-family:system-ui;margin:4rem auto;max-width:32rem;color:#414F50\"><h1 style=\"color:#FF7F00\">{title}</h1><p>{text}</p></body>"
    );
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    ticket
}

/// 32 characters from the OS's randomness (time and addresses mixed in as a fallback).
fn random_state() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    while out.len() < 32 {
        // RandomState is seeded from the OS's random source on every platform std supports.
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(Instant::now().elapsed().as_nanos() ^ std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out.truncate(32);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_callback_with_our_state_carries_the_ticket() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let browser = std::thread::spawn(move || {
            let get = |path: &str| {
                let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
                write!(s, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
                let mut out = String::new();
                let _ = std::io::Read::read_to_string(&mut s, &mut out);
                out
            };
            let wrong = get("/callback?ticket=T1&state=other");
            let right = get("/callback?ticket=T2&state=abcdefghijklmnop");
            (wrong, right)
        });
        let mut got = Vec::new();
        for _ in 0..2 {
            let (stream, _) = listener.accept().unwrap();
            got.push(handle(stream, "abcdefghijklmnop"));
        }
        let (wrong, right) = browser.join().unwrap();
        assert_eq!(got, [None, Some("T2".to_string())]);
        assert!(wrong.contains("Conectare eșuată") && right.contains("Conectat"));
        assert_eq!(random_state().len(), 32);
        assert_ne!(random_state(), random_state());
    }
}
