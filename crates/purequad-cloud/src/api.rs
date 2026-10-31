//! A small blocking client for the cloud-rs REST API. Native clients authenticate with
//! `Authorization: Bearer <session token>`, the same token the browser keeps in its cookie.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Largest file the editors download (the server's own limit is higher; this keeps a bad answer
/// from filling memory).
const MAX_DOWNLOAD: u64 = 2 << 30;

#[derive(Clone, Debug, Deserialize)]
pub struct User {
    pub id: String,
    pub email: String,
    pub display_name: String,
}

/// A file or folder (`GET /api/files/{id}`).
#[derive(Clone, Debug, Deserialize)]
pub struct Node {
    pub id: String,
    pub parent_id: Option<String>,
    pub name: String,
    /// `file` or `folder`.
    pub kind: String,
    pub size_bytes: i64,
    /// `own`, `read` or `write`.
    #[serde(default)]
    pub access: String,
    /// RFC 3339; changes with every new version.
    pub updated_at: String,
}

impl Node {
    pub fn is_folder(&self) -> bool {
        self.kind == "folder"
    }

    pub fn writable(&self) -> bool {
        self.access != "read"
    }
}

#[derive(Deserialize)]
struct Exchanged {
    token: String,
}

#[derive(Deserialize)]
struct UploadSession {
    id: String,
    chunk_size: u64,
}

#[derive(Serialize)]
struct NewUpload<'a> {
    name: &'a str,
    size_bytes: i64,
    mime: Option<&'a str>,
    parent_id: Option<&'a str>,
}

#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    base: String,
    token: String,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").field("base", &self.base).finish_non_exhaustive()
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(Duration::from_secs(600)))
        .http_status_as_error(false)
        .max_redirects(0)
        .user_agent(concat!("purequad-cloud/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

fn net(e: ureq::Error) -> Error {
    Error::Network(e.to_string())
}

/// Turn a non-2xx answer into an [`Error`], reading cloud-rs' `{"error": "..."}` body.
fn check(mut resp: ureq::http::Response<ureq::Body>) -> Result<ureq::http::Response<ureq::Body>> {
    let status = resp.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(resp);
    }
    if status == 401 {
        return Err(Error::SignedOut);
    }
    let body: serde_json::Value = resp.body_mut().with_config().limit(64 * 1024).read_json().unwrap_or_default();
    let message = body
        .get("message")
        .or_else(|| body.get("error"))
        .and_then(|m| m.as_str())
        .unwrap_or_default()
        .to_string();
    Err(Error::Http { status, message })
}

impl Client {
    pub fn new(server: &str, token: &str) -> Self {
        Self { agent: agent(), base: server.trim_end_matches('/').to_string(), token: token.to_string() }
    }

    /// Trade a one-time ticket (from a web link or the browser sign-in) for a session token.
    pub fn exchange_ticket(server: &str, ticket: &str, client_name: &str) -> Result<Self> {
        let agent = agent();
        let base = server.trim_end_matches('/').to_string();
        let resp = agent
            .post(format!("{base}/api/desktop/exchange"))
            .send_json(serde_json::json!({ "ticket": ticket, "client_name": client_name }))
            .map_err(net)?;
        let got: Exchanged = check(resp)?.body_mut().read_json().map_err(net)?;
        Ok(Self { agent, base, token: got.token })
    }

    pub fn server(&self) -> &str {
        &self.base
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    fn get(&self, path: &str) -> Result<ureq::http::Response<ureq::Body>> {
        let resp = self.agent.get(format!("{}{path}", self.base)).header("Authorization", &format!("Bearer {}", self.token)).call().map_err(net)?;
        check(resp)
    }

    pub fn me(&self) -> Result<User> {
        self.get("/api/auth/me")?.body_mut().read_json().map_err(net)
    }

    pub fn node(&self, id: &str) -> Result<Node> {
        self.get(&format!("/api/files/{id}"))?.body_mut().read_json().map_err(net)
    }

    /// A folder's children; `None` lists the user's root.
    pub fn list(&self, parent: Option<&str>) -> Result<Vec<Node>> {
        let path = match parent {
            Some(p) => format!("/api/files?parent_id={p}"),
            None => "/api/files".to_string(),
        };
        self.get(&path)?.body_mut().read_json().map_err(net)
    }

    /// Files and folders other people shared with the user directly.
    pub fn shared_with_me(&self) -> Result<Vec<Node>> {
        #[derive(Deserialize)]
        struct Shared {
            node: Node,
        }
        let shared: Vec<Shared> = self.get("/api/shares/received")?.body_mut().read_json().map_err(net)?;
        Ok(shared.into_iter().map(|s| s.node).collect())
    }

    pub fn download(&self, id: &str) -> Result<Vec<u8>> {
        self.get(&format!("/api/files/{id}/content"))?.body_mut().with_config().limit(MAX_DOWNLOAD).read_to_vec().map_err(net)
    }

    /// Upload `bytes` as a new version of file `id`.
    pub fn upload_version(&self, id: &str, name: &str, bytes: &[u8]) -> Result<Node> {
        self.upload(name, bytes, None, Some(id))
    }

    /// Upload `bytes` as a new file `name` in folder `parent` (`None`: the user's root).
    pub fn upload_new(&self, parent: Option<&str>, name: &str, bytes: &[u8]) -> Result<Node> {
        self.upload(name, bytes, parent, None)
    }

    fn upload(&self, name: &str, bytes: &[u8], parent: Option<&str>, version_of: Option<&str>) -> Result<Node> {
        let auth = format!("Bearer {}", self.token);
        let new = NewUpload { name, size_bytes: i64::try_from(bytes.len()).unwrap_or(i64::MAX), mime: None, parent_id: parent };
        let resp = self.agent.post(format!("{}/api/files/uploads", self.base)).header("Authorization", &auth).send_json(&new).map_err(net)?;
        let session: UploadSession = check(resp)?.body_mut().read_json().map_err(net)?;
        let chunk = usize::try_from(session.chunk_size.max(1)).unwrap_or(8 << 20);
        for (i, part) in bytes.chunks(chunk).enumerate() {
            let resp = self
                .agent
                .put(format!("{}/api/files/uploads/{}/chunks/{i}", self.base, session.id))
                .header("Authorization", &auth)
                .header("Content-Type", "application/octet-stream")
                .send(part)
                .map_err(net)?;
            check(resp)?;
        }
        let resp = self
            .agent
            .post(format!("{}/api/files/uploads/{}/complete", self.base, session.id))
            .header("Authorization", &auth)
            .send_json(serde_json::json!({ "file_id": version_of }))
            .map_err(net)?;
        check(resp)?.body_mut().read_json().map_err(net)
    }
}
