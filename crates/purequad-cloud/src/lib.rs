//! PureQuad Cloud for the PureQuad desktop editors (Docs, Sheets, Slides, PDF).
//!
//! The editors work on ordinary local files. This crate connects such a file to PureQuad Cloud:
//!
//! - [`link`] reads the `purequad-<app>://open?...` addresses that the web app's
//!   "Deschide în aplicația desktop" button hands to the operating system;
//! - [`api`] is a small blocking client for the cloud-rs REST API (Bearer session token);
//! - [`store`] remembers the server and the session token per user;
//! - [`signin`] connects the app to the user's account through the browser;
//! - [`requests`] carries the editors' "PureQuad Cloud" buttons to the app's cloud window;
//! - [`workcopy`] downloads the cloud file into a private working folder and, every time the
//!   editor saves it, uploads the result as a new version of the same cloud file.
//!
//! Nothing here touches the editors' internals, so the same crate serves all four apps: an app
//! only turns a link into a local path ([`open_link`]) and opens that path as it opens any file.

#[cfg(feature = "net")]
pub mod api;
#[cfg(feature = "net")]
pub mod link;
pub mod requests;
#[cfg(feature = "net")]
pub mod signin;
#[cfg(feature = "net")]
pub mod store;
#[cfg(feature = "net")]
pub mod workcopy;

#[cfg(feature = "net")]
pub use api::{Client, Node, User};
#[cfg(feature = "net")]
pub use link::Link;
#[cfg(feature = "net")]
pub use workcopy::{Status, WorkCopy};

/// The cloud the apps use when nothing else is configured.
#[cfg(feature = "net")]
pub const DEFAULT_SERVER: &str = "https://cloud.purequad.com";

/// Everything that can go wrong talking to the cloud, in words fit for the status bar.
#[cfg(feature = "net")]
#[derive(Debug)]
pub enum Error {
    /// No network, TLS failure, timeout.
    Network(String),
    /// The server answered with an error status.
    Http { status: u16, message: String },
    /// The saved sign-in is no longer valid (expired, revoked or wiped).
    SignedOut,
    /// A link that isn't a PureQuad Cloud address.
    BadLink(String),
    Io(std::io::Error),
    Other(String),
}

#[cfg(feature = "net")]
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(e) => write!(f, "PureQuad Cloud can't be reached: {e}"),
            Self::Http { status, message } if message.is_empty() => write!(f, "PureQuad Cloud answered {status}"),
            Self::Http { status, message } => write!(f, "PureQuad Cloud answered {status}: {message}"),
            Self::SignedOut => write!(f, "You are signed out of PureQuad Cloud; open the file again from the cloud"),
            Self::BadLink(e) => write!(f, "Not a PureQuad Cloud link: {e}"),
            Self::Io(e) => write!(f, "{e}"),
            Self::Other(e) => write!(f, "{e}"),
        }
    }
}

#[cfg(feature = "net")]
impl std::error::Error for Error {}

#[cfg(feature = "net")]
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

#[cfg(feature = "net")]
pub type Result<T> = std::result::Result<T, Error>;

/// Handle a `purequad-<app>://` link: sign in with its one-time ticket (when it carries one),
/// download the file into its working folder and start uploading every save as a new version.
///
/// Returns the local path the editor should open and the [`WorkCopy`] that keeps it in sync
/// (dropping it stops the sync). `app_name` is shown in the web app's list of sessions, for
/// example "PureQuad Docs (desktop)". `on_status` is called from the sync thread.
#[cfg(feature = "net")]
pub fn open_link(raw: &str, app_name: &str, on_status: impl Fn(Status) + Send + 'static) -> Result<WorkCopy> {
    let link = Link::parse(raw)?;
    let client = match &link.ticket {
        Some(ticket) => {
            let client = Client::exchange_ticket(&link.server, ticket, &format!("{app_name} (desktop)"))?;
            store::save(&store::Saved { server: link.server.clone(), token: client.token().to_string() })?;
            client
        }
        None => {
            let saved = store::load().filter(|s| s.server == link.server).ok_or(Error::SignedOut)?;
            Client::new(&saved.server, &saved.token)
        }
    };
    let file = link.file.as_deref().ok_or_else(|| Error::BadLink("no file in the link".into()))?;
    WorkCopy::open(client, file, Box::new(on_status))
}

/// Whether a command-line argument (or a macOS URL event) is a PureQuad Cloud link.
#[cfg(feature = "net")]
pub fn is_link(arg: &str) -> bool {
    link::is_link(arg)
}
