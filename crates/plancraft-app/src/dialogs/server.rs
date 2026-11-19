//! File › Open from Server…: the server's address, its key fingerprint on the first visit, user
//! name and password, then the plan (docs/COLLABORATION.md › Phase 3).
//!
//! Network work runs on a background thread, so the window keeps drawing while a server is slow
//! to answer. The password is held only while the window is open and by the plan's link (to sign
//! in again after the connection drops); it is never saved.

use std::sync::Arc;
use std::sync::mpsc;

use egui::{Context, RichText};
use plancraft_server::client::{Client, ClientError};
use plancraft_server::link::{Account, Link, OpenedLink, Waker, normalise_address};
use plancraft_server::protocol::{PlanInfo, Role, Secret};
use plancraft_server::wire::{Fingerprint, Trust, WireError};

use crate::settings::{AppSettings, KnownServer, same_address};
use crate::theme;

/// A plan opened from a server, for the app to show.
pub struct Opened {
    pub opened: OpenedLink,
    pub address: String,
    pub user: String,
    pub plan: String,
}

pub enum Outcome {
    /// Still open.
    Open,
    Closed,
    Opened(Box<Opened>),
}

enum Stage {
    Form,
    /// First visit: the person checks the server's key fingerprint before sending a password.
    Confirm {
        address: String,
        fingerprint: Fingerprint,
        server: String,
    },
    /// Signed in: the plans this person may open.
    Plans {
        address: String,
        fingerprint: Fingerprint,
        server: String,
        plans: Vec<PlanInfo>,
        chosen: Option<String>,
    },
}

enum Done {
    NewServer {
        address: String,
        fingerprint: Fingerprint,
        server: String,
    },
    Plans {
        address: String,
        fingerprint: Fingerprint,
        server: String,
        plans: Vec<PlanInfo>,
    },
    Opened(Box<Opened>),
    /// The server shows another key than the one confirmed before.
    WrongKey {
        address: String,
    },
    Failed(String),
}

pub struct OpenFromServer {
    address: String,
    user: String,
    password: String,
    stage: Stage,
    error: Option<String>,
    /// What the background thread is doing, and where its answer arrives.
    busy: Option<(String, mpsc::Receiver<Done>)>,
    /// The server at this address showed another key: offer to forget the old one.
    wrong_key: Option<String>,
    /// A key confirmed in this window whose sign-in then failed (a mistyped password): signing in
    /// again with it does not ask to confirm it again.
    confirmed: Option<(String, Fingerprint)>,
    /// Listening for servers announced on the network while the window is open (`None`: it
    /// could not, or in tests).
    browser: Option<plancraft_server::discovery::Browser>,
    found: Vec<plancraft_server::discovery::Found>,
}

/// A sentence for a failed connection or sign-in.
fn describe(e: &ClientError, address: &str) -> String {
    match e {
        ClientError::Wire(WireError::Io(io)) => match io.kind() {
            std::io::ErrorKind::ConnectionRefused => {
                format!("No PlanCraft Server answered at {address}. Check the address, and that the server is running.")
            }
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => format!("The server at {address} did not answer in time."),
            _ => format!("Cannot reach {address}: {io}"),
        },
        other => other.to_string(),
    }
}

fn sign_in(address: &str, trust: &Trust, user: &str, password: &str) -> Result<Client, ClientError> {
    let mut client = Client::connect(address, trust, "PlanCraft")?;
    client.login(user, password)?;
    // A server that stops answering must not leave the window waiting for ever.
    client.set_read_timeout(Some(std::time::Duration::from_secs(60)))?;
    Ok(client)
}

impl OpenFromServer {
    /// The window, filled in with the server used last.
    pub fn new(settings: &AppSettings) -> Self {
        let last = settings.servers.first().cloned().unwrap_or_default();
        OpenFromServer {
            address: last.address,
            user: last.user,
            password: String::new(),
            stage: Stage::Form,
            error: None,
            busy: None,
            wrong_key: None,
            confirmed: None,
            // Opening this window is what lets PlanCraft look for servers on the network; it stops
            // when the window closes. Tests never touch the network.
            browser: if cfg!(test) { None } else { plancraft_server::discovery::Browser::start().ok() },
            found: Vec::new(),
        }
    }

    /// The window for a server this PlanCraft started (Share Folder): its key is known, so it is
    /// not asked about.
    pub fn for_own_server(settings: &AppSettings, address: &str, fingerprint: &str) -> Self {
        let mut w = Self::new(settings);
        w.address = address.to_string();
        if let Some(k) = settings.servers.iter().find(|k| same_address(&k.address, address)) {
            w.user = k.user.clone();
        }
        if let Ok(a) = normalise_address(address) {
            w.confirmed = Some((a, Fingerprint(fingerprint.to_string())));
        }
        w
    }

    /// Servers heard of on the network (tests).
    #[cfg(test)]
    pub fn set_found(&mut self, found: Vec<plancraft_server::discovery::Found>) {
        self.found = found;
    }

    fn run(&mut self, ctx: &Context, what: &str, job: impl FnOnce() -> Done + Send + 'static) {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let started = std::thread::Builder::new().name("plancraft-open-from-server".into()).spawn(move || {
            let done = plancraft_core::guard::catch("opening a plan from a server", job)
                .unwrap_or_else(|p| Done::Failed(format!("PlanCraft hit an internal error ({})", p.message)));
            let _ = tx.send(done);
            ctx.request_repaint();
        });
        match started {
            Ok(_) => {
                self.error = None;
                self.busy = Some((what.to_string(), rx));
            }
            Err(e) => self.error = Some(format!("Cannot start connecting: {e}")),
        }
    }

    /// Connects; signs in and lists the plans when the server's key is known.
    fn connect(&mut self, ctx: &Context, settings: &AppSettings) {
        let address = match normalise_address(&self.address) {
            Ok(a) => a,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        // A key confirmed in this window (or known because this PlanCraft runs the server) comes
        // first: a server started again on this computer keeps its key, but a stale pin for the
        // same address must not hide the one just shown.
        let known = self
            .confirmed
            .as_ref()
            .filter(|(a, _)| *a == address)
            .map(|(_, f)| f.clone())
            .or_else(|| settings.fingerprint_for(&address).map(|f| Fingerprint(f.to_string())));
        let (user, password) = (self.user.trim().to_string(), self.password.clone());
        self.run(ctx, &format!("Connecting to {address}…"), move || match known {
            None => match Client::connect(address.as_str(), &Trust::FirstUse, "PlanCraft") {
                Ok(c) => Done::NewServer { address, fingerprint: c.fingerprint.clone(), server: c.server.clone() },
                Err(e) => Done::Failed(describe(&e, &address)),
            },
            Some(fp) => Self::list(address, fp, &user, &password),
        });
    }

    fn list(address: String, fingerprint: Fingerprint, user: &str, password: &str) -> Done {
        match sign_in(&address, &Trust::Pinned(fingerprint.clone()), user, password) {
            Ok(mut c) => match c.list() {
                Ok(plans) => Done::Plans { address, fingerprint, server: c.server.clone(), plans },
                Err(e) => Done::Failed(describe(&e, &address)),
            },
            Err(ClientError::Wire(WireError::WrongServer { .. })) => Done::WrongKey { address },
            Err(e) => Done::Failed(describe(&e, &address)),
        }
    }

    fn open(&mut self, ctx: &Context, address: String, fingerprint: Fingerprint, plan: String) {
        let (user, password) = (self.user.trim().to_string(), self.password.clone());
        let repaint = ctx.clone();
        let waker: Waker = Arc::new(move || repaint.request_repaint());
        self.run(ctx, &format!("Opening {plan}…"), move || {
            let client = match sign_in(&address, &Trust::Pinned(fingerprint.clone()), &user, &password) {
                Ok(c) => c,
                Err(ClientError::Wire(WireError::WrongServer { .. })) => return Done::WrongKey { address },
                Err(e) => return Done::Failed(describe(&e, &address)),
            };
            let account = Account { address: address.clone(), fingerprint, user: user.clone(), password: Secret(password) };
            match Link::open(client, account, &plan, waker) {
                Ok(opened) => Done::Opened(Box::new(Opened { opened, address, user, plan })),
                Err(e) => Done::Failed(e),
            }
        });
    }

    /// Takes the background thread's answer, if it has come.
    fn collect(&mut self, settings: &mut AppSettings) -> Option<Box<Opened>> {
        let (_, rx) = self.busy.as_ref()?;
        let done = match rx.try_recv() {
            Ok(d) => d,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => Done::Failed("Connecting stopped unexpectedly.".into()),
        };
        self.busy = None;
        match done {
            Done::NewServer { address, fingerprint, server } => self.stage = Stage::Confirm { address, fingerprint, server },
            Done::Plans { address, fingerprint, server, plans } => {
                let last = settings.servers.iter().find(|k| same_address(&k.address, &address)).map(|k| k.plan.clone());
                // The key is pinned once the person confirmed it and signed in with it.
                settings.remember_server(KnownServer {
                    address: address.clone(),
                    user: self.user.trim().to_string(),
                    fingerprint: fingerprint.0.clone(),
                    plan: last.clone().unwrap_or_default(),
                    name: server.clone(),
                });
                self.confirmed = None;
                let chosen = last.filter(|l| plans.iter().any(|p| &p.name == l)).or_else(|| plans.first().map(|p| p.name.clone()));
                self.stage = Stage::Plans { address, fingerprint, server, plans, chosen };
            }
            Done::Opened(opened) => {
                if let Some(k) = settings.servers.iter().find(|k| same_address(&k.address, &opened.address)).cloned() {
                    settings.remember_server(KnownServer { plan: opened.plan.clone(), ..k });
                }
                self.password.clear();
                return Some(opened);
            }
            Done::WrongKey { address } => {
                self.stage = Stage::Form;
                self.error = Some(format!(
                    "The server at {address} shows a different key than when you first connected. It may be another computer \
                     pretending to be the server. Ask whoever runs it whether its key changed."
                ));
                self.wrong_key = Some(address);
            }
            Done::Failed(e) => {
                // Signing in after confirming a key failed (a mistyped password, most likely): back
                // to the form to type it again; the key stays confirmed for this window.
                if let Stage::Confirm { address, fingerprint, .. } = &self.stage {
                    self.confirmed = Some((address.clone(), fingerprint.clone()));
                    self.stage = Stage::Form;
                }
                self.error = Some(e);
            }
        }
        None
    }

    pub fn show(&mut self, ctx: &Context, settings: &mut AppSettings) -> Outcome {
        if let Some(opened) = self.collect(settings) {
            return Outcome::Opened(opened);
        }
        if let Some(b) = self.browser.as_mut() {
            if b.poll() {
                self.found = b.found();
            }
            // Announcements arrive on another thread: look again in a moment.
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        let mut open = true;
        let mut cancel = false;
        egui::Window::new("Open from Server")
            .id(egui::Id::new("open_from_server"))
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .pivot(egui::Align2::CENTER_TOP)
            .default_pos(ctx.content_rect().center_top() + egui::vec2(0.0, 120.0))
            .open(&mut open)
            .show(ctx, |ui| {
                let busy = self.busy.is_some();
                match &self.stage {
                    Stage::Form => self.form(ui, ctx, settings, busy),
                    Stage::Confirm { .. } => cancel = self.confirm(ui, ctx, settings, busy),
                    Stage::Plans { .. } => cancel = self.plans(ui, ctx, busy),
                }
                if let Some((what, _)) = &self.busy {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(what.as_str());
                    });
                }
                if let Some(e) = &self.error {
                    ui.add_space(4.0);
                    ui.colored_label(theme::pal().ERROR, e.as_str());
                }
                if let Some(address) = self.wrong_key.clone()
                    && ui.button("Forget the old key").on_hover_text("Only if whoever runs the server says its key changed").clicked()
                {
                    settings.servers.retain(|k| !same_address(&k.address, &address));
                    settings.save();
                    self.wrong_key = None;
                    self.error = None;
                }
            });
        if cancel {
            self.stage = Stage::Form;
        }
        if open { Outcome::Open } else { Outcome::Closed }
    }

    fn form(&mut self, ui: &mut egui::Ui, ctx: &Context, settings: &AppSettings, busy: bool) {
        ui.label("Open a plan shared from a PlanCraft Server on your network.");
        ui.add_space(6.0);
        let mut submit = false;
        egui::Grid::new("server_form").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label("Server:");
            ui.add_enabled(
                !busy,
                egui::TextEdit::singleline(&mut self.address).hint_text("office-pc or 192.168.1.20").desired_width(280.0),
            );
            ui.end_row();
            ui.label("User name:");
            ui.add_enabled(!busy, egui::TextEdit::singleline(&mut self.user).desired_width(280.0));
            ui.end_row();
            ui.label("Password:");
            let r = ui.add_enabled(!busy, egui::TextEdit::singleline(&mut self.password).password(true).desired_width(280.0));
            submit = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.end_row();
        });
        if !self.found.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("On your network:").color(theme::pal().TEXT_WEAK));
                for f in self.found.iter().take(12) {
                    // An address already trusted, if the server gave one, before the best: anyone
                    // can add addresses to another server's announcement.
                    let pinned = |a: &String| settings.fingerprint_for(&normalise_address(a).unwrap_or_default()).is_some();
                    let known = f.addresses.iter().find(|a| pinned(a));
                    let address = known.unwrap_or(&f.address).clone();
                    let name = RichText::new(&f.name);
                    let button = egui::Button::new(name).small().truncate();
                    let r = ui.add_enabled(!busy, button).on_hover_text(format!("{}\n{address}", f.name));
                    // The address, always: two servers can call themselves the same.
                    ui.label(RichText::new(&address).small().color(theme::pal().TEXT_WEAK));
                    // Drawn by PlanCraft, beside the name (a name can't fake it).
                    if known.is_some() {
                        ui.label(RichText::new("✔").small().color(theme::pal().OK)).on_hover_text("Its key is already trusted here");
                    }
                    if r.clicked() {
                        self.address = address.clone();
                        if let Some(k) = settings.servers.iter().find(|k| same_address(&k.address, &address)) {
                            self.user = k.user.clone();
                        }
                    }
                }
            });
        } else if self.browser.is_some() {
            ui.label(RichText::new("Looking for servers on your network…").small().color(theme::pal().TEXT_WEAK));
        }
        if settings.servers.len() > 1 {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Recent:").color(theme::pal().TEXT_WEAK));
                for k in settings.servers.iter().take(5) {
                    if ui.small_button(&k.address).clicked() {
                        self.address = k.address.clone();
                        self.user = k.user.clone();
                    }
                }
            });
        }
        ui.add_space(6.0);
        let ready = !busy && !self.address.trim().is_empty() && !self.user.trim().is_empty() && !self.password.is_empty();
        let clicked = ui
            .add_enabled(ready, egui::Button::new(RichText::new("Connect").color(egui::Color32::WHITE)).fill(theme::pal().ACCENT))
            .clicked();
        if ready && (clicked || submit) {
            self.wrong_key = None;
            self.connect(ctx, settings);
        }
    }

    /// Returns true when cancelled.
    fn confirm(&mut self, ui: &mut egui::Ui, ctx: &Context, settings: &AppSettings, busy: bool) -> bool {
        let Stage::Confirm { address, fingerprint, server } = &self.stage else { return false };
        let (address, fingerprint) = (address.clone(), fingerprint.clone());
        if let Some(known) = settings.same_name_other_key(server, &fingerprint.0) {
            // Not a first visit at all, perhaps: the same name at another address, another key.
            ui.colored_label(
                theme::pal().ERROR,
                format!(
                    "Careful: a server named {server} you used before, at {}, had a different key. This may be another computer \
                     pretending to be it. Unless whoever runs it says it moved and its key changed, choose It doesn't match.",
                    known.address
                ),
            );
        } else {
            ui.label(format!("This is your first visit to {server} ({address})."));
        }
        ui.label("Check that this key fingerprint matches the one the server shows (ask whoever runs it):");
        ui.add_space(4.0);
        egui::Frame::new().fill(theme::pal().HEADER_BG).corner_radius(4.0).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
            ui.label(RichText::new(&fingerprint.0).monospace().size(14.0));
        });
        ui.label(
            RichText::new("PlanCraft remembers it, and refuses a server showing another key from then on.")
                .small()
                .color(theme::pal().TEXT_WEAK),
        );
        ui.add_space(6.0);
        let mut cancel = false;
        ui.horizontal(|ui| {
            let ok = egui::Button::new(RichText::new("It matches: sign in").color(egui::Color32::WHITE)).fill(theme::pal().ACCENT);
            if ui.add_enabled(!busy, ok).clicked() {
                let (user, password) = (self.user.trim().to_string(), self.password.clone());
                self.run(ctx, "Signing in…", move || Self::list(address, fingerprint, &user, &password));
            }
            cancel = ui.add_enabled(!busy, egui::Button::new("It doesn't match")).clicked();
        });
        if cancel {
            self.error = Some("Not connected: the key did not match. Ask whoever runs the server.".into());
        }
        cancel
    }

    /// Returns true when the person goes back to the form.
    fn plans(&mut self, ui: &mut egui::Ui, ctx: &Context, busy: bool) -> bool {
        let Stage::Plans { address, fingerprint, server, plans, chosen } = &mut self.stage else { return false };
        ui.label(format!("Signed in to {server} as {}.", self.user.trim()));
        ui.add_space(4.0);
        let mut open_now = None;
        if plans.is_empty() {
            ui.label(RichText::new("There are no plans you may open on this server.").color(theme::pal().TEXT_WEAK));
        }
        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
            for p in plans.iter() {
                let selected = chosen.as_deref() == Some(p.name.as_str());
                let people = match p.people {
                    0 => String::new(),
                    1 => " · 1 person in it".into(),
                    n => format!(" · {n} people in it"),
                };
                let role = if p.role == Role::Viewer { " · view only" } else { "" };
                let r = ui.selectable_label(selected, format!("📄 {}{role}{people}", p.name));
                if r.clicked() {
                    *chosen = Some(p.name.clone());
                }
                if r.double_clicked() {
                    open_now = Some(p.name.clone());
                }
            }
        });
        ui.add_space(6.0);
        let mut back = false;
        ui.horizontal(|ui| {
            let ok = egui::Button::new(RichText::new("Open").color(egui::Color32::WHITE)).fill(theme::pal().ACCENT);
            if ui.add_enabled(!busy && chosen.is_some(), ok).clicked() {
                open_now = chosen.clone();
            }
            back = ui.add_enabled(!busy, egui::Button::new("Back")).clicked();
        });
        if let Some(plan) = open_now.filter(|_| !busy) {
            let (address, fingerprint) = (address.clone(), fingerprint.clone());
            self.open(ctx, address, fingerprint, plan);
        }
        back
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::time::{Duration, Instant};

    use super::*;
    use crate::remote::test_server::{PASSWORD, TestServer};

    /// Waits for the background thread's answer.
    fn settle(d: &mut OpenFromServer, settings: &mut AppSettings) -> Option<Box<Opened>> {
        let end = Instant::now() + Duration::from_secs(20);
        while d.busy.is_some() {
            if let Some(o) = d.collect(settings) {
                return Some(o);
            }
            assert!(Instant::now() < end, "no answer from the server");
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    fn filled(settings: &AppSettings, server: &TestServer, user: &str, password: &str) -> OpenFromServer {
        let mut d = OpenFromServer::new(settings);
        d.address = server.address();
        d.user = user.into();
        d.password = password.into();
        d
    }

    #[test]
    fn a_first_visit_checks_the_key_then_signs_in_and_opens_a_plan() {
        let server = TestServer::start();
        let ctx = Context::default();
        let mut settings = AppSettings::default();
        let mut d = filled(&settings, &server, "ana", PASSWORD);
        d.connect(&ctx, &settings);
        assert!(settle(&mut d, &mut settings).is_none());
        let Stage::Confirm { address, fingerprint, .. } = &d.stage else { panic!("the key is shown first: {:?}", d.error) };
        assert_eq!(fingerprint, server.running.fingerprint());
        assert!(settings.servers.is_empty(), "nothing is remembered before the person confirms");
        let (address, fingerprint) = (address.clone(), fingerprint.clone());
        let (a, f) = (address.clone(), fingerprint.clone());
        d.run(&ctx, "Signing in…", move || OpenFromServer::list(a, f, "ana", PASSWORD));
        settle(&mut d, &mut settings);
        let Stage::Plans { plans, chosen, .. } = &d.stage else { panic!("{:?}", d.error) };
        assert!(plans.iter().any(|p| p.name == "Plan.plancraft"));
        assert_eq!(chosen.as_deref(), Some("Plan.plancraft"));
        assert_eq!(settings.fingerprint_for(&address), Some(fingerprint.0.as_str()), "the key is remembered");
        d.open(&ctx, address.clone(), fingerprint, "Plan.plancraft".into());
        let opened = settle(&mut d, &mut settings).expect("opened");
        assert_eq!(opened.opened.role, Role::Editor);
        assert!(!opened.opened.project.tasks.is_empty());
        assert_eq!(settings.servers[0].plan, "Plan.plancraft");
        assert!(d.password.is_empty(), "the window forgets the password");
        assert!(!serde_json::to_string(&settings).unwrap().contains(PASSWORD), "never saved");

        // The next visit goes straight to the plans, filled in with the last server.
        let mut again = OpenFromServer::new(&settings);
        assert_eq!((again.address.as_str(), again.user.as_str()), (address.as_str(), "ana"));
        again.password = PASSWORD.into();
        again.connect(&ctx, &settings);
        settle(&mut again, &mut settings);
        assert!(matches!(again.stage, Stage::Plans { .. }), "{:?}", again.error);
    }

    #[test]
    fn a_wrong_password_or_another_key_is_refused() {
        let server = TestServer::start();
        let ctx = Context::default();
        let mut settings = AppSettings::default();
        settings.servers.push(KnownServer {
            address: server.address(),
            user: "ana".into(),
            fingerprint: server.running.fingerprint().0.clone(),
            plan: String::new(),
            name: String::new(),
        });
        let mut d = filled(&settings, &server, "ana", "not the password");
        d.connect(&ctx, &settings);
        settle(&mut d, &mut settings);
        assert!(matches!(d.stage, Stage::Form));
        assert!(d.error.as_deref().unwrap_or_default().contains("wrong user name or password"), "{:?}", d.error);

        // The server shows another key than the one confirmed: refused before any password.
        settings.servers[0].fingerprint = "0000 0000 0000 0000 0000 0000 0000 0000".into();
        let mut d = filled(&settings, &server, "ana", PASSWORD);
        d.connect(&ctx, &settings);
        settle(&mut d, &mut settings);
        assert!(d.error.as_deref().unwrap_or_default().contains("different key"), "{:?}", d.error);
        assert_eq!(d.wrong_key.as_deref(), Some(server.address().as_str()));

        let mut d = filled(&settings, &server, "ana", PASSWORD);
        d.address = "127.0.0.1:1".into();
        d.connect(&ctx, &settings);
        settle(&mut d, &mut settings);
        assert!(d.error.is_some());
    }

    #[test]
    fn a_mistyped_password_on_a_first_visit_goes_back_to_the_form() {
        let server = TestServer::start();
        let ctx = Context::default();
        let mut settings = AppSettings::default();
        let mut d = filled(&settings, &server, "ana", "mistyped");
        d.connect(&ctx, &settings);
        settle(&mut d, &mut settings);
        let Stage::Confirm { address, fingerprint, .. } = &d.stage else { panic!("{:?}", d.error) };
        let (a, f) = (address.clone(), fingerprint.clone());
        d.run(&ctx, "Signing in…", move || OpenFromServer::list(a, f, "ana", "mistyped"));
        settle(&mut d, &mut settings);
        assert!(matches!(d.stage, Stage::Form), "back to the form, where the password can be typed again");
        assert!(d.error.as_deref().unwrap_or_default().contains("wrong user name or password"), "{:?}", d.error);
        // Typed again: straight to the plans, without confirming the key a second time.
        d.password = PASSWORD.into();
        d.connect(&ctx, &settings);
        settle(&mut d, &mut settings);
        assert!(matches!(d.stage, Stage::Plans { .. }), "{:?}", d.error);
    }

    #[test]
    fn a_server_this_plancraft_runs_opens_without_asking_about_its_key() {
        // Share Folder knows its server's key, so it is trusted as is, even over an old pin for
        // the same address (a server run here before with another key).
        let server = TestServer::start();
        let ctx = Context::default();
        let mut settings = AppSettings::default();
        settings.servers.push(KnownServer {
            address: server.address(),
            user: "ana".into(),
            fingerprint: "0000 0000 0000 0000 0000 0000 0000 0000".into(),
            plan: String::new(),
            name: String::new(),
        });
        let mut d = OpenFromServer::for_own_server(&settings, &server.address(), &server.running.fingerprint().0);
        assert_eq!(d.user, "ana", "the user name used there before is filled in");
        d.password = PASSWORD.into();
        d.connect(&ctx, &settings);
        settle(&mut d, &mut settings);
        assert!(matches!(d.stage, Stage::Plans { .. }), "{:?}", d.error);
        assert_eq!(settings.fingerprint_for(&server.address()), Some(server.running.fingerprint().0.as_str()));
    }

    #[test]
    fn servers_found_on_the_network_fill_in_the_address() {
        use egui_kittest::kittest::Queryable;
        let mut d = OpenFromServer::new(&AppSettings::default());
        d.address.clear();
        d.set_found(vec![
            plancraft_server::discovery::Found {
                name: "Plans on studio-pc".into(),
                address: "10.0.0.2".into(),
                addresses: vec!["10.0.0.2".into()],
            },
            plancraft_server::discovery::Found {
                name: "Office on desk-7".into(),
                address: "10.0.0.7:47400".into(),
                addresses: vec!["10.0.0.7:47400".into()],
            },
        ]);
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1000.0, 800.0)).build_ui_state(
            |ui, (d, settings): &mut (OpenFromServer, AppSettings)| {
                let _ = d.show(ui.ctx(), settings);
            },
            (d, AppSettings::default()),
        );
        h.run();
        h.get_by_label("Office on desk-7").click();
        h.run();
        assert_eq!(h.state().0.address, "10.0.0.7:47400");
        // Only the address: the key is still checked on the first visit.
        assert!(h.state().0.confirmed.is_none());
    }

    /// Regression: anyone on the network can add an address to another server's announcement;
    /// the address already trusted is used, not the "best" one an impostor added.
    #[test]
    fn a_trusted_address_wins_over_one_added_by_someone_else() {
        use egui_kittest::kittest::Queryable;
        let mut settings = AppSettings::default();
        settings.servers.push(crate::settings::KnownServer {
            address: "192.168.1.9".into(),
            user: "ana".into(),
            fingerprint: "aa bb".into(),
            ..Default::default()
        });
        let mut d = OpenFromServer::new(&settings);
        d.address.clear();
        d.set_found(vec![plancraft_server::discovery::Found {
            name: "Plans on studio-pc".into(),
            address: "1.0.0.1".into(),
            addresses: vec!["1.0.0.1".into(), "192.168.1.9".into()],
        }]);
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1000.0, 800.0)).build_ui_state(
            |ui, (d, settings): &mut (OpenFromServer, AppSettings)| {
                let _ = d.show(ui.ctx(), settings);
            },
            (d, settings),
        );
        h.run();
        h.get_by_label("Plans on studio-pc").click();
        h.run();
        assert_eq!(h.state().0.address, "192.168.1.9");
        assert_eq!(h.state().0.user, "ana");
    }
}
