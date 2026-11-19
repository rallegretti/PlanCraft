//! File › Share Folder…: shares a folder of plans with people on the network, through a PlanCraft
//! Server this PlanCraft starts (docs/COLLABORATION.md › Phase 4), and manages who may sign in.
//!
//! The server runs as a separate program (`plancraft serve FOLDER --watch-stdin`, see
//! `plancraft_server::sharing`): it has its own lock identity, so it refuses a plan open in one
//! of this PlanCraft's windows, and it stops by itself, saving every plan, when PlanCraft quits.
//! Nothing is shared, and nothing announced on the network, until someone clicks Start Sharing.
//!
//! People are kept in the server's users file (the same one `plancraft serve user …` manages);
//! the server reads it again when it changes, so changes here apply at once, also to people
//! signed in now. Passwords are hashed on a background thread (Argon2id takes a moment) and
//! never kept once the change is made.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use egui::{Context, RichText};
use plancraft_core::io::DiskStamp;
use plancraft_server::protocol::{DEFAULT_PORT, Role};
use plancraft_server::server::users_path;
use plancraft_server::sharing::{Program, Sharing, State};
use plancraft_server::users::{User, Users};

use crate::settings::AppSettings;
use crate::theme;

/// The server this PlanCraft runs, if any, and why the last one stopped on its own.
#[derive(Default)]
pub struct ShareHost {
    pub sharing: Option<Sharing>,
    /// The last server stopped by itself (could not start, or crashed): why.
    pub error: Option<String>,
}

impl ShareHost {
    /// Looks at the server. Returns why it stopped, once, when it stopped on its own.
    pub fn poll(&mut self) -> Option<String> {
        let state = self.sharing.as_mut()?.state();
        match state {
            State::Stopped(why) => {
                self.sharing = None;
                self.error = why.clone();
                why
            }
            _ => None,
        }
    }

    /// The folder being shared and where the server is.
    pub fn status(&mut self) -> Option<(PathBuf, State)> {
        let s = self.sharing.as_mut()?;
        let state = s.state();
        Some((s.folder().to_path_buf(), state))
    }

    /// Starts sharing `folder`.
    pub fn start(&mut self, program: &Program, folder: &Path, config: &Path, port: u16, announce: bool, ctx: &Context) {
        let repaint = ctx.clone();
        let waker: plancraft_server::sharing::Waker = std::sync::Arc::new(move || repaint.request_repaint());
        self.error = None;
        match Sharing::start(program, folder, config, port, announce, waker) {
            Ok(s) => self.sharing = Some(s),
            Err(e) => self.error = Some(e),
        }
    }

    /// Asks the server to stop (it saves every plan first).
    pub fn stop(&mut self) {
        if let Some(s) = self.sharing.as_mut() {
            s.stop();
        }
    }
}

/// The program that runs the server: this PlanCraft as `plancraft serve`. From an AppImage, the
/// AppImage file itself (the executable sits in a temporary mount that goes away when this
/// PlanCraft quits, while the server is still saving its plans).
pub fn server_program() -> Result<Program, String> {
    match std::env::var_os("APPIMAGE").filter(|a| !a.is_empty()) {
        Some(a) => Ok(Program { program: PathBuf::from(a), args: vec!["serve".into()] }),
        None => Program::this_plancraft(),
    }
}

pub enum Outcome {
    Open,
    Closed,
    /// Open a plan from this server in this PlanCraft: its address here and its key fingerprint
    /// (known, since this PlanCraft started it).
    OpenFromServer {
        address: String,
        fingerprint: String,
    },
}

/// A change to the users file, made on a background thread.
#[derive(Clone, Debug)]
enum UserOp {
    Add { name: String, password: String, role: Role },
    Password { name: String, password: String },
    Role { name: String, role: Role, plan: Option<String> },
    ClearPlan { name: String, plan: String },
    Remove { name: String },
}

/// Makes a change to the users file; returns what to say.
fn apply(path: &Path, op: UserOp) -> Result<String, String> {
    let mut users = Users::load(path)?;
    let said = match op {
        UserOp::Add { name, password, role } => {
            users.add(&name, &password, role)?;
            format!("Added {} ({}).", name.trim(), role.name())
        }
        UserOp::Password { name, password } => {
            users.set_password(&name, &password)?;
            format!("Changed the password of {name}.")
        }
        UserOp::Role { name, role, plan: None } => {
            users.set_role(&name, role, None)?;
            format!("{name} is now {}.", role.name())
        }
        UserOp::Role { name, role, plan: Some(plan) } => {
            users.set_role(&name, role, Some(&plan))?;
            format!("{name} is now {} for {plan}.", role.name())
        }
        UserOp::ClearPlan { name, plan } => {
            users.clear_plan_role(&name, &plan)?;
            format!("{name} has the same access to {plan} as to the other plans again.")
        }
        UserOp::Remove { name } => {
            users.remove(&name)?;
            format!("Removed {name}.")
        }
    };
    users.save()?;
    Ok(said)
}

/// The `.plancraft` files directly in `folder` (what the server shares), by name.
fn plans_in(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| plancraft_server::server::valid_plan_name(n))
                .take(10_000)
                .collect()
        })
        .unwrap_or_default();
    names.sort_by_key(|n| n.to_lowercase());
    names
}

fn role_label(role: Role) -> &'static str {
    match role {
        Role::None => "No access",
        Role::Viewer => "Viewer",
        Role::Editor => "Editor",
        Role::Admin => "Admin",
    }
}

fn role_combo(ui: &mut egui::Ui, id: impl std::hash::Hash + std::fmt::Debug, role: &mut Role, with_none: bool, enabled: bool) -> bool {
    let before = *role;
    ui.add_enabled_ui(enabled, |ui| {
        egui::ComboBox::from_id_salt(id).selected_text(role_label(*role)).width(96.0).show_ui(ui, |ui| {
            let roles: &[Role] =
                if with_none { &[Role::None, Role::Viewer, Role::Editor, Role::Admin] } else { &[Role::Viewer, Role::Editor, Role::Admin] };
            for r in roles {
                ui.selectable_value(role, *r, role_label(*r));
            }
        });
    });
    *role != before
}

/// A coloured dot (the "●" glyph is missing from the default fonts).
pub fn status_dot(ui: &mut egui::Ui, color: egui::Color32) {
    let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 12.0), egui::Sense::hover());
    ui.painter().circle_filled(dot.center(), 4.0, color);
}

fn copy_box(ui: &mut egui::Ui, text: &str) {
    ui.horizontal(|ui| {
        egui::Frame::new().fill(theme::pal().HEADER_BG).corner_radius(4.0).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
            ui.label(RichText::new(text).monospace());
        });
        if ui.small_button("📋").on_hover_text("Copy").clicked() {
            ui.ctx().copy_text(text.to_string());
        }
    });
}

/// The window.
pub struct ShareFolder {
    /// The server's private folder (its key, users file and trusted sign-ins).
    config: PathBuf,
    folder: Option<PathBuf>,
    port: String,
    plans: Vec<String>,
    /// The users file as last read, and its stamp then.
    users: Result<Vec<User>, String>,
    users_stamp: Option<Option<DiskStamp>>,
    new_name: String,
    new_password: String,
    new_role: Role,
    /// Someone whose password is being changed, and the new one.
    password_for: Option<(String, String)>,
    /// Someone the person asked to remove (asked again before it happens).
    remove_asked: Option<String>,
    /// A per-plan rule being added: who, the plan and the role.
    new_rule: Option<(String, String, Role)>,
    /// A change being made to the users file, and where its answer arrives.
    busy: Option<(String, mpsc::Receiver<Result<String, String>>)>,
    /// What the last change said (`Err`: it failed).
    note: Option<Result<String, String>>,
    /// The addresses shown for the server, worked out once for its port.
    addresses: Option<(u16, Vec<String>)>,
}

impl ShareFolder {
    /// The window, with the folder shared last (or `suggested`, a folder whose plan was found
    /// with a server's journal beside it).
    pub fn new(settings: &AppSettings, config: PathBuf, suggested: Option<PathBuf>) -> Self {
        let folder = suggested.or_else(|| settings.share_folder.clone());
        let plans = folder.as_deref().map(plans_in).unwrap_or_default();
        let mut w = ShareFolder {
            config,
            folder,
            port: settings.share_port.to_string(),
            plans,
            users: Ok(Vec::new()),
            users_stamp: None,
            new_name: String::new(),
            new_password: String::new(),
            new_role: Role::Editor,
            password_for: None,
            remove_asked: None,
            new_rule: None,
            busy: None,
            note: None,
            addresses: None,
        };
        w.reload_users();
        w
    }

    fn users_file(&self) -> PathBuf {
        users_path(&self.config)
    }

    /// Reads the users file again if it changed (another window, or the command line).
    fn reload_users(&mut self) {
        let path = self.users_file();
        let stamp = plancraft_core::io::disk_stamp(&path);
        if self.users_stamp.as_ref() == Some(&stamp) {
            return;
        }
        self.users = Users::load(&path).map(|u| u.list().to_vec());
        self.users_stamp = Some(stamp);
    }

    fn change(&mut self, ctx: &Context, what: &str, op: UserOp) {
        if self.busy.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let path = self.users_file();
        let repaint = ctx.clone();
        let started = std::thread::Builder::new().name("plancraft-share-users".into()).spawn(move || {
            let done = plancraft_core::guard::catch("changing who may sign in", || apply(&path, op))
                .unwrap_or_else(|p| Err(format!("PlanCraft hit an internal error ({})", p.message)));
            let _ = tx.send(done);
            repaint.request_repaint();
        });
        match started {
            Ok(_) => {
                self.note = None;
                self.busy = Some((what.to_string(), rx));
            }
            Err(e) => self.note = Some(Err(format!("Cannot change who may sign in: {e}"))),
        }
    }

    /// Takes the answer of a change, if it has come.
    fn collect(&mut self) {
        let Some((_, rx)) = &self.busy else { return };
        let done = match rx.try_recv() {
            Ok(d) => d,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("The change stopped unexpectedly.".into()),
        };
        self.busy = None;
        if done.is_ok() {
            self.new_name.clear();
            self.new_password.clear();
            self.password_for = None;
            self.new_rule = None;
        }
        self.users_stamp = None;
        self.reload_users();
        self.note = Some(done);
    }

    /// Draws the window. `open_here`: the plans of the shared folder open in this PlanCraft (the
    /// server can't share them until they are closed here).
    pub fn show(
        &mut self,
        ctx: &Context,
        settings: &mut AppSettings,
        host: &mut ShareHost,
        open_here: &dyn Fn(&Path) -> Vec<String>,
    ) -> Outcome {
        self.collect();
        self.reload_users();
        let mut open = true;
        let mut outcome = Outcome::Open;
        egui::Window::new("Share Folder")
            .id(egui::Id::new("share_folder"))
            .collapsible(false)
            .resizable(false)
            .default_width(560.0)
            .pivot(egui::Align2::CENTER_TOP)
            .default_pos(ctx.content_rect().center_top() + egui::vec2(0.0, 90.0))
            .open(&mut open)
            .show(ctx, |ui| {
                if let Some(o) = self.server(ui, ctx, settings, host, open_here) {
                    outcome = o;
                }
                ui.add_space(6.0);
                ui.separator();
                self.people(ui, ctx);
                if let Some(s) = host.sharing.as_ref() {
                    let log = s.log();
                    if !log.is_empty() {
                        ui.add_space(4.0);
                        egui::CollapsingHeader::new("Server log").id_salt("share_log").show(ui, |ui| {
                            egui::ScrollArea::vertical().max_height(160.0).stick_to_bottom(true).show(ui, |ui| {
                                for line in log.iter().rev().take(200).rev() {
                                    ui.label(RichText::new(line).monospace().size(11.0));
                                }
                            });
                        });
                    }
                }
            });
        if !open {
            return Outcome::Closed;
        }
        outcome
    }

    /// The top half: the folder and the server.
    fn server(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &Context,
        settings: &mut AppSettings,
        host: &mut ShareHost,
        open_here: &dyn Fn(&Path) -> Vec<String>,
    ) -> Option<Outcome> {
        let status = host.status();
        let mut outcome = None;
        match status {
            Some((folder, state)) if !matches!(state, State::Stopped(_)) => {
                let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| folder.display().to_string());
                match &state {
                    State::Starting => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(format!("Starting to share {name}…"));
                        });
                    }
                    State::Stopping => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(format!("Stopping: saving the plans of {name}…"));
                        });
                    }
                    State::Sharing(ready) => {
                        ui.horizontal(|ui| {
                            status_dot(ui, theme::pal().OK);
                            ui.label(RichText::new(format!("Sharing {name}")).strong());
                        })
                        .response
                        .on_hover_text(folder.display().to_string());
                        let port = ready.addr.port();
                        ui.label("People open its plans with File › Open from Server… at one of these addresses:");
                        if self.addresses.as_ref().is_none_or(|(p, _)| *p != port) {
                            let host_name = gethostname_short();
                            let mut list = Vec::new();
                            if !host_name.is_empty() {
                                list.push(if port == DEFAULT_PORT { host_name } else { format!("{host_name}:{port}") });
                            }
                            list.extend(plancraft_server::discovery::local_addresses(port));
                            self.addresses = Some((port, list));
                        }
                        let addresses = self.addresses.as_ref().map(|(_, a)| a.clone()).unwrap_or_default();
                        for a in &addresses {
                            copy_box(ui, a);
                        }
                        if settings.share_announce {
                            ui.label(
                                RichText::new("It is also listed under On your network in their Open from Server window.")
                                    .small()
                                    .color(theme::pal().TEXT_WEAK),
                            );
                        }
                        ui.add_space(4.0);
                        ui.label("On their first visit they check that the server's key fingerprint matches this one:");
                        copy_box(ui, &ready.fingerprint);
                        let here = open_here(&folder);
                        if !here.is_empty() {
                            ui.add_space(4.0);
                            ui.colored_label(
                                theme::pal().WARN,
                                format!(
                                    "Open in this PlanCraft, so not shared yet: {}. Close them here, then open them from the server \
                                     (Open a Plan from It) to work on them with the others.",
                                    here.join(", ")
                                ),
                            );
                        }
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            let open_btn = egui::Button::new(RichText::new("Open a Plan from It…").color(egui::Color32::WHITE))
                                .fill(theme::pal().ACCENT);
                            if ui.add(open_btn).on_hover_text("Work on its plans here, alongside the others").clicked() {
                                outcome = Some(Outcome::OpenFromServer {
                                    address: format!("127.0.0.1:{port}"),
                                    fingerprint: ready.fingerprint.clone(),
                                });
                            }
                            if ui.button("Stop Sharing").on_hover_text("Saves every plan and tells everyone in them").clicked() {
                                host.stop();
                            }
                        });
                        ui.label(RichText::new("Sharing stops when you quit PlanCraft.").small().color(theme::pal().TEXT_WEAK));
                    }
                    State::Stopped(_) => {}
                }
            }
            _ => self.not_sharing(ui, ctx, settings, host),
        }
        outcome
    }

    fn not_sharing(&mut self, ui: &mut egui::Ui, ctx: &Context, settings: &mut AppSettings, host: &mut ShareHost) {
        ui.label(
            "Share a folder of plans so people on your network can open them with File › Open from Server… and work on them \
             together. Nothing is shared until you start.",
        );
        ui.add_space(6.0);
        egui::Grid::new("share_form").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label("Folder:");
            ui.horizontal(|ui| {
                match &self.folder {
                    Some(f) => ui.label(RichText::new(f.display().to_string()).monospace()),
                    None => ui.label(RichText::new("None chosen").color(theme::pal().TEXT_WEAK)),
                };
                if ui.button("Choose…").clicked()
                    && let Some(f) = rfd::FileDialog::new().pick_folder()
                {
                    self.plans = plans_in(&f);
                    self.folder = Some(f);
                }
            });
            ui.end_row();
            ui.label("Port:");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.port).desired_width(60.0));
                ui.checkbox(&mut settings.share_announce, "List it for others on the network");
            });
            ui.end_row();
        });
        if let Some(f) = &self.folder {
            let n = self.plans.len();
            let text = match n {
                0 => "No plans in this folder yet: plans saved into it are shared too.".to_string(),
                1 => "1 plan in this folder.".to_string(),
                n => format!("{n} plans in this folder."),
            };
            ui.label(RichText::new(text).small().color(theme::pal().TEXT_WEAK)).on_hover_text(f.display().to_string());
        }
        let port = self.port.trim().parse::<u16>().ok().filter(|p| *p != 0);
        let no_users = self.users.as_ref().is_ok_and(Vec::is_empty);
        if no_users {
            ui.colored_label(theme::pal().WARN, "Add at least one person below first (yourself too, to open its plans here).");
        }
        if port.is_none() {
            ui.colored_label(theme::pal().ERROR, "The port is a number from 1 to 65535.");
        }
        if let Some(e) = &host.error {
            ui.colored_label(theme::pal().ERROR, format!("Sharing stopped: {e}"));
        }
        ui.add_space(4.0);
        let ready = self.folder.is_some() && port.is_some() && self.users.as_ref().is_ok_and(|u| !u.is_empty());
        let start = egui::Button::new(RichText::new("Start Sharing").color(egui::Color32::WHITE)).fill(theme::pal().ACCENT);
        if ui.add_enabled(ready, start).clicked()
            && let (Some(folder), Some(port)) = (self.folder.clone(), port)
        {
            match server_program() {
                Ok(program) => host.start(&program, &folder, &self.config, port, settings.share_announce, ctx),
                Err(e) => host.error = Some(e),
            }
            settings.share_folder = Some(folder);
            settings.share_port = port;
            settings.save();
        }
    }

    /// The bottom half: who may sign in.
    fn people(&mut self, ui: &mut egui::Ui, ctx: &Context) {
        ui.label(RichText::new("People who may sign in").strong());
        let busy = self.busy.is_some();
        let users = match &self.users {
            Ok(u) => u.clone(),
            Err(e) => {
                ui.colored_label(theme::pal().ERROR, format!("Cannot read the list of people: {e}"));
                return;
            }
        };
        if users.is_empty() {
            ui.label(RichText::new("Nobody yet.").color(theme::pal().TEXT_WEAK));
        }
        let mut op: Option<(String, UserOp)> = None;
        egui::ScrollArea::vertical().id_salt("share_people_scroll").max_height(280.0).show(ui, |ui| {
            egui::Grid::new("share_people").num_columns(3).spacing([10.0, 6.0]).striped(true).show(ui, |ui| {
                for u in &users {
                    ui.label(&u.name);
                    let mut role = u.role;
                    if role_combo(ui, ("share_role", &u.name), &mut role, false, !busy) {
                        op = Some((format!("Changing {}'s role…", u.name), UserOp::Role { name: u.name.clone(), role, plan: None }));
                    }
                    ui.horizontal(|ui| {
                        if ui.add_enabled(!busy, egui::Button::new("Password…").small()).clicked() {
                            self.password_for = Some((u.name.clone(), String::new()));
                        }
                        if ui
                            .add_enabled(!busy, egui::Button::new("Plans…").small())
                            .on_hover_text("Other access to single plans")
                            .clicked()
                        {
                            self.new_rule = Some((u.name.clone(), self.plans.first().cloned().unwrap_or_default(), Role::None));
                        }
                        if self.remove_asked.as_deref() == Some(u.name.as_str()) {
                            if ui
                                .add_enabled(!busy, egui::Button::new(RichText::new("Remove for good").color(theme::pal().ERROR)).small())
                                .clicked()
                            {
                                op = Some((format!("Removing {}…", u.name), UserOp::Remove { name: u.name.clone() }));
                                self.remove_asked = None;
                            }
                            if ui.small_button("Keep").clicked() {
                                self.remove_asked = None;
                            }
                        } else if ui.add_enabled(!busy, egui::Button::new("Remove").small()).clicked() {
                            self.remove_asked = Some(u.name.clone());
                        }
                    });
                    ui.end_row();
                    for (plan, role) in &u.plans {
                        ui.label(RichText::new(format!("    {plan}")).small().color(theme::pal().TEXT_WEAK));
                        ui.label(RichText::new(role_label(*role)).small());
                        if ui.add_enabled(!busy, egui::Button::new("Same as other plans").small()).clicked() {
                            op = Some((
                                format!("Changing {}'s access to {plan}…", u.name),
                                UserOp::ClearPlan { name: u.name.clone(), plan: plan.clone() },
                            ));
                        }
                        ui.end_row();
                    }
                }
            });
        });
        if let Some((name, password)) = self.password_for.as_mut() {
            ui.add_space(4.0);
            let name = name.clone();
            let mut submit = false;
            let mut cancel = false;
            ui.horizontal(|ui| {
                ui.label(format!("New password for {name}:"));
                let r = ui.add_enabled(!busy, egui::TextEdit::singleline(password).password(true).desired_width(160.0));
                submit = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                submit |= ui.add_enabled(!busy && !password.is_empty(), egui::Button::new("Change")).clicked();
                cancel = ui.button("Cancel").clicked();
            });
            if submit && !password.is_empty() {
                op = Some((format!("Changing {name}'s password…"), UserOp::Password { name, password: password.clone() }));
            } else if cancel {
                self.password_for = None;
            }
        }
        if let Some((name, plan, role)) = self.new_rule.as_mut() {
            ui.add_space(4.0);
            let mut add = false;
            let mut cancel = false;
            ui.horizontal(|ui| {
                ui.label(format!("{name} for"));
                if self.plans.is_empty() {
                    ui.add(egui::TextEdit::singleline(plan).hint_text("Plan.plancraft").desired_width(160.0));
                } else {
                    egui::ComboBox::from_id_salt("share_rule_plan").selected_text(plan.as_str()).width(180.0).show_ui(ui, |ui| {
                        for p in &self.plans {
                            ui.selectable_value(plan, p.clone(), p);
                        }
                    });
                }
                ui.label("is");
                role_combo(ui, "share_rule_role", role, true, !busy);
                let valid = plancraft_server::server::valid_plan_name(plan);
                add = ui.add_enabled(!busy && valid, egui::Button::new("Set")).clicked();
                cancel = ui.button("Cancel").clicked();
            });
            if add {
                op = Some((
                    format!("Changing {name}'s access to {plan}…"),
                    UserOp::Role { name: name.clone(), role: *role, plan: Some(plan.clone()) },
                ));
            } else if cancel {
                self.new_rule = None;
            }
        }
        ui.add_space(6.0);
        let mut submit = false;
        ui.horizontal_wrapped(|ui| {
            ui.label("Add:");
            ui.add_enabled(!busy, egui::TextEdit::singleline(&mut self.new_name).hint_text("Name").desired_width(120.0));
            let r = ui.add_enabled(
                !busy,
                egui::TextEdit::singleline(&mut self.new_password).hint_text("Password").password(true).desired_width(130.0),
            );
            submit = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let mut role = self.new_role;
            if role_combo(ui, "share_new_role", &mut role, false, !busy) {
                self.new_role = role;
            }
            submit |= ui
                .add_enabled(!busy && !self.new_name.trim().is_empty() && !self.new_password.is_empty(), egui::Button::new("Add"))
                .clicked();
        });
        if submit && !self.new_name.trim().is_empty() && !self.new_password.is_empty() && op.is_none() {
            let name = self.new_name.trim().to_string();
            op = Some((format!("Adding {name}…"), UserOp::Add { name, password: self.new_password.clone(), role: self.new_role }));
        }
        ui.label(
            RichText::new(format!(
                "Passwords need at least {} characters. Viewers open plans; editors also change them. Changes apply at once, also to \
                 people signed in now.",
                plancraft_server::users::MIN_PASSWORD_CHARS
            ))
            .small()
            .color(theme::pal().TEXT_WEAK),
        );
        if let Some((what, _)) = &self.busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(what.as_str());
            });
        }
        match &self.note {
            Some(Ok(t)) => {
                ui.colored_label(theme::pal().OK, t.as_str());
            }
            Some(Err(e)) => {
                ui.colored_label(theme::pal().ERROR, e.as_str());
            }
            None => {}
        }
        if let Some((what, op)) = op {
            self.change(ctx, &what, op);
        }
    }
}

/// This computer's name, as people type it.
fn gethostname_short() -> String {
    let name = plancraft_server::server::host_name();
    name.split('.').next().unwrap_or_default().trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait(w: &mut ShareFolder) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while w.busy.is_some() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(20));
            w.collect();
        }
    }

    #[test]
    fn people_are_added_changed_and_removed() {
        let dir = crate::recovery::tests::TempDir::new("share-users");
        let config = dir.0.as_path().join("server");
        let ctx = Context::default();
        let mut w = ShareFolder::new(&AppSettings::default(), config.clone(), Some(dir.0.as_path().to_path_buf()));
        assert!(w.users.as_ref().unwrap().is_empty());

        w.change(&ctx, "", UserOp::Add { name: "Ana".into(), password: "short".into(), role: Role::Editor });
        wait(&mut w);
        assert!(matches!(&w.note, Some(Err(e)) if e.contains("8 characters")), "{:?}", w.note);

        w.new_name = "Ana".into();
        w.new_password = "correct horse".into();
        w.change(&ctx, "", UserOp::Add { name: "Ana".into(), password: "correct horse".into(), role: Role::Admin });
        wait(&mut w);
        assert!(matches!(&w.note, Some(Ok(_))), "{:?}", w.note);
        assert!(w.new_password.is_empty(), "the password is not kept once used");
        assert_eq!(w.users.as_ref().unwrap().len(), 1);

        w.change(&ctx, "", UserOp::Role { name: "Ana".into(), role: Role::None, plan: Some("Secret.plancraft".into()) });
        wait(&mut w);
        let ana = w.users.as_ref().unwrap()[0].clone();
        assert_eq!(ana.role, Role::Admin);
        assert_eq!(ana.role_for("Secret.plancraft"), Role::None);
        w.change(&ctx, "", UserOp::ClearPlan { name: "Ana".into(), plan: "Secret.plancraft".into() });
        wait(&mut w);
        assert_eq!(w.users.as_ref().unwrap()[0].role_for("Secret.plancraft"), Role::Admin);

        w.change(&ctx, "", UserOp::Password { name: "Ana".into(), password: "battery staple".into() });
        wait(&mut w);
        let users = Users::load(&users_path(&config)).unwrap();
        assert!(users.verify("ana", "battery staple").is_some());
        let text = std::fs::read_to_string(users_path(&config)).unwrap();
        assert!(!text.contains("battery staple") && !text.contains("correct horse"));

        // Someone added from the command line meanwhile shows up.
        let mut users = Users::load(&users_path(&config)).unwrap();
        users.add("Ben", "correct horse", Role::Viewer).unwrap();
        users.save().unwrap();
        w.users_stamp = None;
        w.reload_users();
        assert_eq!(w.users.as_ref().unwrap().len(), 2);

        w.change(&ctx, "", UserOp::Remove { name: "Ana".into() });
        wait(&mut w);
        assert_eq!(w.users.as_ref().unwrap().iter().map(|u| u.name.as_str()).collect::<Vec<_>>(), ["Ben"]);
    }

    #[test]
    fn only_plans_directly_in_the_folder_are_listed() {
        let dir = crate::recovery::tests::TempDir::new("share-plans");
        std::fs::write(dir.0.as_path().join("B.plancraft"), "{}").unwrap();
        std::fs::write(dir.0.as_path().join("a.plancraft"), "{}").unwrap();
        std::fs::write(dir.0.as_path().join(".hidden.plancraft"), "{}").unwrap();
        std::fs::write(dir.0.as_path().join("notes.txt"), "").unwrap();
        std::fs::create_dir(dir.0.as_path().join("sub")).unwrap();
        assert_eq!(plans_in(dir.0.as_path()), ["a.plancraft", "B.plancraft"]);
        assert!(plans_in(&dir.0.as_path().join("missing")).is_empty());
    }

    #[test]
    fn the_window_adds_people_and_says_why_sharing_stopped() {
        use egui_kittest::kittest::Queryable;
        let dir = crate::recovery::tests::TempDir::new("share-window");
        let config = dir.0.join("server");
        let settings = AppSettings::default();
        let w = ShareFolder::new(&settings, config.clone(), Some(dir.0.clone()));
        let host = ShareHost { sharing: None, error: Some("cannot listen on 0.0.0.0:47340: address in use".into()) };
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1000.0, 800.0)).build_ui_state(
            |ui, (w, settings, host): &mut (ShareFolder, AppSettings, ShareHost)| {
                let _ = w.show(ui.ctx(), settings, host, &|_| Vec::new());
            },
            (w, settings, host),
        );
        h.run();
        assert!(h.query_by_label_contains("address in use").is_some(), "why the last server stopped is shown");
        assert!(h.query_by_label_contains("Add at least one person").is_some());
        h.state_mut().0.new_name = "Ana".into();
        h.state_mut().0.new_password = "correct horse".into();
        h.get_by_label("Add").click();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            h.step();
            if h.state().0.busy.is_none() {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        h.run();
        assert!(h.query_by_label_contains("Added Ana").is_some());
        assert!(h.query_by_label_contains("Add at least one person").is_none());
        assert!(Users::load(&users_path(&config)).unwrap().get("ana").is_some());
    }
}
