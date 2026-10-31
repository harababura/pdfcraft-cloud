//! PureQuad Cloud in the app (fork patch). Same file in PureQuad Docs, Sheets, Slides and PDF
//! (purequad-cloud/glue/cloud_links.rs); it compiles against egui 0.36 and 0.37.
//!
//! - Links: the web app's "Deschide în aplicația desktop" hands the system a
//!   `purequad-<app>://open?…` link: on Windows and Linux it arrives as a command-line argument,
//!   on macOS as a GetURL Apple event (`apple_events`).
//! - The PureQuad Cloud window (title-bar cloud button, File menu): sign in through the browser,
//!   browse "Fișierele mele" and "Partajate cu mine", open a file, or save a copy of the open
//!   document into a cloud folder.
//!
//! Downloads and uploads run on their own threads. A cloud file opens as a local working copy
//! like any file, and every save goes back to the cloud as a new version
//! (`purequad_cloud::WorkCopy`).

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};

use purequad_cloud::requests::Request;
use purequad_cloud::{Client, Node, User};

pub use purequad_cloud::is_link;

/// What the app does with the next queued message.
pub enum Action {
    /// Open this local working copy like a file from File › Open.
    Open(PathBuf),
    /// Show this in the status bar (sync progress or a problem).
    Status(String),
}

enum Msg {
    Ready(purequad_cloud::WorkCopy),
    Status(String),
    SignedIn(Client, User),
    Listed(u64, purequad_cloud::Result<Vec<Node>>),
    Saved(String),
    Failed(String),
    SignedOut,
}

/// Which folder the window shows.
#[derive(Clone, PartialEq)]
enum Place {
    Mine,
    Shared,
    Folder { id: String, name: String, writable: bool },
}

enum Login {
    /// Not known yet (the saved sign-in is being checked).
    Checking,
    Out,
    Waiting(purequad_cloud::signin::SignIn),
    In(Client, User),
}

struct Window {
    open: bool,
    /// The local file to save to the cloud; `None` when browsing to open.
    save: Option<PathBuf>,
    name: String,
    path: Vec<Place>,
    items: Option<Vec<Node>>,
    generation: u64,
    busy: bool,
    error: Option<String>,
    replace: Option<Node>,
}

pub struct CloudLinks {
    app_name: &'static str,
    /// Extensions the app opens (lower case, no dot); other files are shown dimmed.
    extensions: &'static [&'static str],
    ro: bool,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    ctx: Option<egui::Context>,
    /// Links that came before the window had a context (command line, launch event).
    waiting: Vec<String>,
    /// The open working copies: each keeps its file in sync until the app quits.
    copies: Vec<purequad_cloud::WorkCopy>,
    login: Login,
    win: Window,
}

fn server() -> String {
    std::env::var("PUREQUAD_CLOUD_SERVER").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| purequad_cloud::DEFAULT_SERVER.to_string())
}

/// Romanian unless the system says otherwise (most PureQuad colleagues work in Romanian).
fn romanian() -> bool {
    for k in ["PUREQUAD_LANG", "LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(v) = std::env::var(k) {
            if !v.is_empty() && v != "C" && v != "POSIX" {
                return v.to_ascii_lowercase().starts_with("ro");
            }
        }
    }
    true
}

impl CloudLinks {
    pub fn new(app_name: &'static str) -> Self {
        let (tx, rx) = channel();
        let extensions: &'static [&'static str] = match app_name {
            "PureQuad Docs" => &["docx", "doc", "odt", "rtf", "txt", "md", "html", "htm"],
            "PureQuad Sheets" => &["xlsx", "xlsm", "xls", "ods", "csv", "tsv"],
            "PureQuad Slides" => &["pptx", "ppt", "odp"],
            "PureQuad PDF" => &["pdf"],
            _ => &[],
        };
        Self {
            app_name,
            extensions,
            ro: romanian(),
            tx,
            rx,
            ctx: None,
            waiting: Vec::new(),
            copies: Vec::new(),
            login: Login::Checking,
            win: Window {
                open: false,
                save: None,
                name: String::new(),
                path: vec![Place::Mine],
                items: None,
                generation: 0,
                busy: false,
                error: None,
                replace: None,
            },
        }
    }

    fn t<'a>(&self, ro: &'a str, en: &'a str) -> &'a str {
        if self.ro { ro } else { en }
    }

    fn spawn(&self, name: &str, job: impl FnOnce() -> Msg + Send + 'static) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        let spawned = std::thread::Builder::new().name(name.into()).spawn(move || {
            let _ = tx.send(job());
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
        if let Err(e) = spawned {
            log::error!("PureQuad Cloud: no thread: {e}");
        }
    }

    fn status_fn(&self) -> Box<dyn Fn(purequad_cloud::Status) + Send> {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        Box::new(move |s: purequad_cloud::Status| {
            let _ = tx.send(Msg::Status(s.to_string()));
            if let Some(ctx) = &ctx {
                ctx.request_repaint();
            }
        })
    }

    /// Start opening `link` in the background.
    pub fn start(&mut self, link: &str) {
        if self.ctx.is_none() {
            self.waiting.push(link.to_string());
            return;
        }
        let (link, app_name, on_status) = (link.to_string(), self.app_name, self.status_fn());
        self.spawn("purequad-cloud-open", move || match purequad_cloud::open_link(&link, app_name, on_status) {
            Ok(copy) => Msg::Ready(copy),
            Err(e) => {
                log::warn!("PureQuad Cloud link: {e}");
                Msg::Status(e.to_string())
            }
        });
    }

    /// Show the PureQuad Cloud window to open a file from the cloud.
    pub fn browse(&mut self) {
        self.show_window(None);
    }

    /// Show the PureQuad Cloud window to save a copy of `path` (the open document) to the cloud.
    pub fn save_copy(&mut self, path: PathBuf) {
        self.show_window(Some(path));
    }

    fn show_window(&mut self, save: Option<PathBuf>) {
        self.win.name = save.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.win.save = save;
        self.win.open = true;
        self.win.error = None;
        self.win.replace = None;
        if matches!(self.login, Login::Checking) {
            match purequad_cloud::signin::saved_client() {
                Some(client) if client.server() == server().trim_end_matches('/') => {
                    self.spawn("purequad-cloud-me", move || match client.me() {
                        Ok(user) => Msg::SignedIn(client, user),
                        Err(purequad_cloud::Error::SignedOut) => Msg::SignedOut,
                        Err(e) => Msg::Failed(e.to_string()),
                    });
                }
                _ => self.login = Login::Out,
            }
        } else {
            // Fresh every time: files change in the cloud (and through this window).
            self.reload();
        }
    }

    fn reload(&mut self) {
        let Login::In(client, _) = &self.login else { return };
        self.win.generation += 1;
        self.win.items = None;
        self.win.replace = None;
        let (client, generation, place) = (client.clone(), self.win.generation, self.win.path.last().cloned().unwrap_or(Place::Mine));
        self.spawn("purequad-cloud-list", move || {
            let listed = match place {
                Place::Mine => client.list(None),
                Place::Shared => client.shared_with_me(),
                Place::Folder { id, .. } => client.list(Some(&id)),
            };
            Msg::Listed(generation, listed)
        });
    }

    fn open_file(&mut self, node: &Node) {
        let Login::In(client, _) = &self.login else { return };
        let (client, id, on_status) = (client.clone(), node.id.clone(), self.status_fn());
        self.win.open = false;
        self.spawn("purequad-cloud-open", move || match purequad_cloud::WorkCopy::open(client, &id, on_status) {
            Ok(copy) => Msg::Ready(copy),
            Err(e) => Msg::Status(e.to_string()),
        });
    }

    /// Upload the open document into the folder shown (or over `replace`), then open the cloud
    /// file's working copy so the next saves go there.
    fn save_here(&mut self, replace: Option<Node>) {
        let (Login::In(client, _), Some(local)) = (&self.login, self.win.save.clone()) else { return };
        let name = self.win.name.trim().to_string();
        let parent = match self.win.path.last() {
            Some(Place::Folder { id, .. }) => Some(id.clone()),
            _ => None,
        };
        let (client, on_status) = (client.clone(), self.status_fn());
        let saved_text = self.t("Salvat în PureQuad Cloud", "Saved to PureQuad Cloud").to_string();
        self.win.busy = true;
        self.spawn("purequad-cloud-save", move || {
            let bytes = match std::fs::read(&local) {
                Ok(b) => b,
                Err(e) => return Msg::Failed(format!("{}: {e}", local.display())),
            };
            let node = match &replace {
                Some(old) => client.upload_version(&old.id, &name, &bytes),
                None => client.upload_new(parent.as_deref(), &name, &bytes),
            };
            let node = match node {
                Ok(n) => n,
                Err(e) => return Msg::Failed(e.to_string()),
            };
            match purequad_cloud::WorkCopy::open(client, &node.id, on_status) {
                Ok(copy) => Msg::Ready(copy),
                Err(_) => Msg::Saved(format!("{saved_text}: {}", node.name)),
            }
        });
    }

    fn sign_in(&mut self) {
        let ctx = self.ctx.clone();
        let wake = move || {
            if let Some(ctx) = &ctx {
                ctx.request_repaint();
            }
        };
        let open = |url: &str| {
            if let Some(ctx) = &self.ctx {
                ctx.open_url(egui::OpenUrl::new_tab(url));
            }
        };
        match purequad_cloud::signin::start(&server(), self.app_name, open, wake) {
            Ok(s) => self.login = Login::Waiting(s),
            Err(e) => self.win.error = Some(e.to_string()),
        }
    }

    fn sign_out(&mut self) {
        purequad_cloud::store::clear();
        self.login = Login::Out;
        self.win.items = None;
        self.win.path = vec![Place::Mine];
    }

    /// Call every frame. Draws the PureQuad Cloud window when it is open and returns what the
    /// app should do now.
    pub fn poll(&mut self, ctx: &egui::Context) -> Vec<Action> {
        if self.ctx.is_none() {
            self.ctx = Some(ctx.clone());
            for link in std::mem::take(&mut self.waiting) {
                self.start(&link);
            }
        }
        for r in purequad_cloud::requests::take() {
            match r {
                Request::Browse => self.browse(),
                Request::SaveCopy(p) if purequad_cloud::workcopy::is_work_copy(&p) => {
                    let text = self.t(
                        "Documentul e deja în PureQuad Cloud: fiecare salvare ajunge acolo ca versiune nouă.",
                        "This document is in PureQuad Cloud already: every save becomes a new version there.",
                    );
                    self.win.open = false;
                    let _ = self.tx.send(Msg::Status(text.to_string()));
                }
                Request::SaveCopy(p) => self.save_copy(p),
                Request::SaveUnsaved => {
                    let text = self.t(
                        "Salvează întâi documentul pe calculator, apoi alege din nou „Salvează în PureQuad Cloud”.",
                        "Save the document on this computer first, then choose \u{201c}Save to PureQuad Cloud\u{201d} again.",
                    );
                    let _ = self.tx.send(Msg::Status(text.to_string()));
                }
            }
        }
        if let Login::Waiting(s) = &self.login {
            match s.poll() {
                Some(Ok(client)) => {
                    self.login = Login::Checking;
                    self.spawn("purequad-cloud-me", move || match client.me() {
                        Ok(user) => Msg::SignedIn(client, user),
                        Err(e) => Msg::Failed(e.to_string()),
                    });
                }
                Some(Err(e)) => {
                    self.login = Login::Out;
                    self.win.error = Some(e.to_string());
                }
                None => {}
            }
        }
        let mut actions = Vec::new();
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Ready(copy) => {
                    self.win.busy = false;
                    if self.win.save.is_some() {
                        self.win.open = false;
                    }
                    actions.push(Action::Open(copy.path().to_path_buf()));
                    self.copies.push(copy);
                }
                Msg::Status(s) => actions.push(Action::Status(s)),
                Msg::SignedIn(client, user) => {
                    self.login = Login::In(client, user);
                    self.win.error = None;
                    self.reload();
                }
                Msg::Listed(generation, listed) if generation == self.win.generation => match listed {
                    Ok(mut items) => {
                        items.sort_by_key(|n| (!n.is_folder(), n.name.to_lowercase()));
                        self.win.items = Some(items);
                    }
                    Err(purequad_cloud::Error::SignedOut) => self.sign_out(),
                    Err(e) => {
                        self.win.items = Some(Vec::new());
                        self.win.error = Some(e.to_string());
                    }
                },
                Msg::Listed(..) => {}
                Msg::Saved(s) => {
                    self.win.busy = false;
                    self.win.open = false;
                    actions.push(Action::Status(s));
                }
                Msg::Failed(e) => {
                    self.win.busy = false;
                    if matches!(self.login, Login::Checking) {
                        self.login = Login::Out;
                    }
                    self.win.error = Some(e);
                }
                Msg::SignedOut => self.sign_out(),
            }
        }
        if self.win.open {
            self.window(ctx);
        }
        actions
    }

    fn window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let title = if self.win.save.is_some() {
            self.t("Salvează în PureQuad Cloud", "Save to PureQuad Cloud")
        } else {
            self.t("Deschide din PureQuad Cloud", "Open from PureQuad Cloud")
        };
        egui::Window::new(title)
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([560.0, 440.0])
            .show(ctx, |ui| self.contents(ui));
        if !open {
            self.win.open = false;
            if matches!(self.login, Login::Waiting(_)) {
                self.login = Login::Out;
            }
        }
    }

    fn contents(&mut self, ui: &mut egui::Ui) {
        enum Do {
            Nothing,
            SignIn,
            CancelSignIn,
            SignOut,
            Go(usize),
            Enter(Place),
            Open(Node),
            Save(Option<Node>),
            AskReplace(Node),
            Reload,
        }
        let mut todo = Do::Nothing;
        if let Some(e) = &self.win.error {
            ui.colored_label(ui.visuals().error_fg_color, e);
            ui.add_space(6.0);
        }
        match &self.login {
            Login::Checking => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(self.t("Se verifică conectarea…", "Checking the sign-in…"));
                });
            }
            Login::Out => {
                ui.label(self.t(
                    "Conectează aplicația la contul tău PureQuad Cloud. Se deschide browserul, unde confirmi conectarea; parola nu trece prin aplicație.",
                    "Connect the app to your PureQuad Cloud account. Your browser opens to confirm; your password never goes through the app.",
                ));
                ui.add_space(8.0);
                if ui.button(self.t("Conectează-te", "Sign in")).clicked() {
                    todo = Do::SignIn;
                }
                ui.add_space(4.0);
                ui.weak(server());
            }
            Login::Waiting(s) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(self.t("Confirmă conectarea în browser…", "Confirm the sign-in in your browser…"));
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button(self.t("Renunță", "Cancel")).clicked() {
                        todo = Do::CancelSignIn;
                    }
                    ui.hyperlink_to(self.t("Deschide pagina din nou", "Open the page again"), &s.url);
                });
            }
            Login::In(_, user) => {
                ui.horizontal(|ui| {
                    ui.label(format!("{} · {}", user.display_name, user.email));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button(self.t("Deconectează", "Sign out")).clicked() {
                            todo = Do::SignOut;
                        }
                        if ui.small_button("⟳").on_hover_text(self.t("Reîncarcă", "Reload")).clicked() {
                            todo = Do::Reload;
                        }
                    });
                });
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    let root = matches!(self.win.path.first(), Some(Place::Mine));
                    if ui.selectable_label(root && self.win.path.len() == 1, self.t("Fișierele mele", "My files")).clicked() {
                        todo = Do::Enter(Place::Mine);
                    }
                    if ui.selectable_label(!root && self.win.path.len() == 1, self.t("Partajate cu mine", "Shared with me")).clicked() {
                        todo = Do::Enter(Place::Shared);
                    }
                    for (i, p) in self.win.path.iter().enumerate().skip(1) {
                        ui.label("›");
                        if let Place::Folder { name, .. } = p {
                            if ui.selectable_label(i + 1 == self.win.path.len(), name).clicked() {
                                todo = Do::Go(i);
                            }
                        }
                    }
                });
                ui.separator();
                let saving = self.win.save.is_some();
                let bottom = if saving { 64.0 } else { 8.0 };
                egui::ScrollArea::vertical().auto_shrink([false, false]).max_height((ui.available_height() - bottom).max(80.0)).show(ui, |ui| match &self.win.items {
                    None => {
                        ui.spinner();
                    }
                    Some(items) if items.is_empty() => {
                        ui.weak(self.t("Folderul e gol.", "This folder is empty."));
                    }
                    Some(items) => {
                        for n in items {
                            if n.is_folder() {
                                let r = ui.selectable_label(false, format!("📁  {}", n.name));
                                if r.clicked() {
                                    todo = Do::Enter(Place::Folder { id: n.id.clone(), name: n.name.clone(), writable: n.writable() });
                                }
                                continue;
                            }
                            let ext = n.name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
                            let ours = self.extensions.is_empty() || self.extensions.contains(&ext.as_str());
                            let lock = if n.writable() { "" } else { "  🔒" };
                            let text = format!("📄  {}{lock}", n.name);
                            if !ours {
                                ui.add_enabled(false, egui::Button::selectable(false, text));
                                continue;
                            }
                            let r = ui.selectable_label(saving && self.win.name == n.name, text);
                            if saving {
                                if r.clicked() {
                                    self.win.name = n.name.clone();
                                }
                            } else if r.double_clicked() || r.clicked() {
                                todo = Do::Open(n.clone());
                            }
                        }
                    }
                });
                if saving {
                    ui.separator();
                    let place = self.win.path.last().cloned().unwrap_or(Place::Mine);
                    let can_write = match &place {
                        Place::Mine => true,
                        Place::Shared => false,
                        Place::Folder { writable, .. } => *writable,
                    };
                    ui.horizontal(|ui| {
                        ui.label(self.t("Nume:", "Name:"));
                        ui.add(egui::TextEdit::singleline(&mut self.win.name).desired_width(280.0));
                        let ok = can_write && !self.win.busy && !self.win.name.trim().is_empty();
                        if self.win.busy {
                            ui.spinner();
                        }
                        let label = self.t("Salvează aici", "Save here");
                        let r = ui.add_enabled(ok, egui::Button::new(label));
                        let r = if can_write { r } else { r.on_disabled_hover_text(self.t("Alege un folder în care poți scrie.", "Pick a folder you can write to.")) };
                        if r.clicked() {
                            let same = self.win.items.as_ref().and_then(|items| items.iter().find(|n| !n.is_folder() && n.name.eq_ignore_ascii_case(self.win.name.trim())).cloned());
                            todo = match same {
                                Some(old) => Do::AskReplace(old),
                                None => Do::Save(None),
                            };
                        }
                    });
                    if let Some(old) = self.win.replace.clone() {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(if self.ro {
                                format!("„{}” există deja. Îl înlocuiești cu o versiune nouă? (versiunile vechi rămân în istoric)", old.name)
                            } else {
                                format!("\u{201c}{}\u{201d} exists already. Replace it with a new version? (older versions stay in the history)", old.name)
                            });
                            if ui.add_enabled(old.writable() && !self.win.busy, egui::Button::new(self.t("Înlocuiește", "Replace"))).clicked() {
                                todo = Do::Save(Some(old.clone()));
                            }
                            if ui.button(self.t("Renunță", "Cancel")).clicked() {
                                self.win.replace = None;
                            }
                        });
                    }
                }
            }
        }
        match todo {
            Do::Nothing => {}
            Do::SignIn => {
                self.win.error = None;
                self.sign_in();
            }
            Do::CancelSignIn => self.login = Login::Out,
            Do::SignOut => self.sign_out(),
            Do::Go(i) => {
                self.win.path.truncate(i + 1);
                self.reload();
            }
            Do::Enter(p @ (Place::Mine | Place::Shared)) => {
                self.win.path = vec![p];
                self.reload();
            }
            Do::Enter(p) => {
                self.win.path.push(p);
                self.reload();
            }
            Do::Open(n) => self.open_file(&n),
            Do::Save(old) => {
                self.win.replace = None;
                self.win.error = None;
                self.save_here(old);
            }
            Do::AskReplace(old) => self.win.replace = Some(old),
            Do::Reload => self.reload(),
        }
    }
}
