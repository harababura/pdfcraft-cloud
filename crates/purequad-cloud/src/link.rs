//! `purequad-<app>://open?server=<url>&file=<id>&ticket=<one-time ticket>`
//!
//! The web app builds these for "Deschide în aplicația desktop". Each editor registers its own
//! scheme (`purequad-docs`, `purequad-sheets`, `purequad-slides`, `purequad-pdf`) so the right app
//! opens; the generic `purequad://` is accepted too. The ticket is single-use and lives for a
//! minute, so a link that leaks (browser history, logs) can't sign anyone in later.

use crate::{DEFAULT_SERVER, Error, Result};

/// The schemes the apps register.
pub const SCHEMES: &[&str] = &["purequad", "purequad-docs", "purequad-sheets", "purequad-slides", "purequad-pdf"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// `https://cloud.purequad.com`, without a trailing slash.
    pub server: String,
    /// The cloud file's id.
    pub file: Option<String>,
    /// One-time sign-in ticket.
    pub ticket: Option<String>,
}

pub fn is_link(arg: &str) -> bool {
    arg.split_once("://").is_some_and(|(scheme, _)| SCHEMES.iter().any(|s| s.eq_ignore_ascii_case(scheme)))
}

impl Link {
    pub fn parse(raw: &str) -> Result<Self> {
        let raw = raw.trim();
        if !is_link(raw) {
            return Err(Error::BadLink(raw.chars().take(60).collect()));
        }
        let url = url::Url::parse(raw).map_err(|e| Error::BadLink(e.to_string()))?;
        let action = url.host_str().unwrap_or_default();
        if action != "open" {
            return Err(Error::BadLink(format!("unknown action {action:?}")));
        }
        let mut link = Link { server: DEFAULT_SERVER.to_string(), file: None, ticket: None };
        for (k, v) in url.query_pairs() {
            match k.as_ref() {
                "server" => link.server = server_url(&v)?,
                "file" if is_id(&v) => link.file = Some(v.into_owned()),
                "file" => return Err(Error::BadLink("malformed file id".into())),
                "ticket" if !v.is_empty() && v.len() <= 200 => link.ticket = Some(v.into_owned()),
                _ => {}
            }
        }
        Ok(link)
    }
}

/// Only `https://` servers (plain `http://` just for localhost, for development).
fn server_url(raw: &str) -> Result<String> {
    let url = url::Url::parse(raw).map_err(|e| Error::BadLink(e.to_string()))?;
    let host = url.host_str().unwrap_or_default();
    let local = matches!(host, "localhost" | "127.0.0.1");
    match url.scheme() {
        "https" => {}
        "http" if local => {}
        s => return Err(Error::BadLink(format!("{s}: servers are not allowed"))),
    }
    if host.is_empty() || !url.username().is_empty() || url.password().is_some() {
        return Err(Error::BadLink("malformed server".into()));
    }
    let port = url.port().map(|p| format!(":{p}")).unwrap_or_default();
    Ok(format!("{}://{host}{port}", url.scheme()))
}

/// A UUID as cloud-rs prints it.
fn is_id(s: &str) -> bool {
    s.len() == 36 && s.chars().enumerate().all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0b7c8f3e-1d2a-4c5b-9e8f-112233445566";

    #[test]
    fn parses_an_open_link() {
        let l = Link::parse(&format!("purequad-docs://open?server=https%3A%2F%2Fcloud.purequad.com%2F&file={ID}&ticket=abc")).unwrap();
        assert_eq!(l, Link { server: "https://cloud.purequad.com".into(), file: Some(ID.into()), ticket: Some("abc".into()) });
        assert!(is_link("PureQuad-PDF://open?file=x"));
        assert!(!is_link("/Users/me/report.docx"));
        assert!(!is_link("https://cloud.purequad.com"));
    }

    #[test]
    fn defaults_to_the_purequad_cloud() {
        let l = Link::parse(&format!("purequad://open?file={ID}")).unwrap();
        assert_eq!(l.server, DEFAULT_SERVER);
        assert_eq!(l.ticket, None);
    }

    #[test]
    fn refuses_unsafe_servers_and_odd_ids() {
        for bad in [
            format!("purequad://open?server=http://evil.example&file={ID}"),
            format!("purequad://open?server=https://user:pw@evil.example&file={ID}"),
            format!("purequad://open?server=file:///etc&file={ID}"),
            "purequad://open?file=../../etc/passwd".to_string(),
            format!("purequad://delete?file={ID}"),
        ] {
            assert!(Link::parse(&bad).is_err(), "{bad}");
        }
        assert_eq!(Link::parse(&format!("purequad://open?server=http://localhost:8080&file={ID}")).unwrap().server, "http://localhost:8080");
    }
}
