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
use plancraft_server::background::{self, Background};
use plancraft_server::cli::Ready;
use plancraft_server::login;
use plancraft_server::protocol::{DEFAULT_PORT, Role};
use plancraft_server::server::users_path;
use plancraft_server::sharing::{Program, Sharing, State, Wake};
use plancraft_server::users::{NewPassword, User, Users};

use crate::settings::AppSettings;
use crate::theme;

/// The server this PlanCraft runs, if any, and why the last one stopped on its own.
#[derive(Default)]
pub struct ShareHost {
    pub sharing: Option<Sharing>,
    /// The last server stopped by itself (could not start, or crashed), or could not be started:
    /// what to say, in full.
    pub error: Option<String>,
    /// The last server's log, kept once it stopped (to see why it could not start).
    pub last_log: Vec<String>,
    /// Someone is looking at the server's log (the window is open with it unfolded): only then
    /// does a new log line redraw the app, at most a few times a second.
    log_shown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// To look again while the server starts or stops.
    ctx: Option<Context>,
    /// The server running now was started announcing itself on the network.
    pub announced: bool,
    /// A change to the users file still being made when the window closed: its answer is
    /// still told (and the window takes it back when reopened).
    pending: Option<(String, Answer)>,
    /// The server's private folder, where a server kept sharing in the background says where it
    /// is (`None`: nothing is looked for).
    pub config: Option<PathBuf>,
    /// That server as last looked at.
    seen: Option<Seen>,
    /// Starting or stopping it, on a thread: which, and where its answer arrives.
    task: Option<(Task, mpsc::Receiver<Result<(), String>>)>,
    /// Asked to keep sharing this folder at this moment: said to be starting until it says
    /// where it listens.
    asked_start: Option<(std::time::Instant, PathBuf)>,
    /// How the server is started at login (tests stand in for the system's).
    login: LoginItems,
    /// What to tell once, window or not: the background server stopped on its own, or did not
    /// start.
    notice: Option<String>,
    /// Cancel was clicked while the login item was being set: stop once that is done.
    stop_after_start: bool,
    /// The folder being stopped (for what is shown while it stops).
    stopping: Option<PathBuf>,
}

/// Adjusts a command before it runs ([`Program::prepare`]).
type Prepare = Option<fn(&mut std::process::Command)>;

/// Starting the server at login: the system's ([`login`]), or a stand-in in tests.
#[derive(Clone, Copy)]
pub struct LoginItems {
    pub enable: fn(&login::Entry) -> Result<(), String>,
    pub disable: fn(Prepare) -> Result<(), String>,
    pub enabled: fn() -> bool,
}

impl Default for LoginItems {
    fn default() -> Self {
        LoginItems { enable: login::enable, disable: login::disable, enabled: login::enabled }
    }
}

/// A server kept sharing in the background, as last looked at.
struct Seen {
    at: std::time::Instant,
    state: Background,
    /// The system starts it at login.
    at_login: bool,
    /// It wrote a log.
    has_log: bool,
    /// Its last lines, read only while someone looks at them (`None`: not read).
    log: Option<Vec<String>>,
}

/// What is being done to the server kept sharing in the background.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Task {
    Start,
    Stop,
    /// Only taken out of the login items (it is not running).
    Forget,
}

/// How often the server kept sharing in the background is looked at (it reads the disk).
const BACKGROUND_LOOK: std::time::Duration = std::time::Duration::from_secs(1);
/// How long a server asked to keep sharing may take to say where it listens.
const BACKGROUND_START_WAIT: std::time::Duration = std::time::Duration::from_secs(20);
/// Log lines shown.
const LOG_SHOWN: usize = 200;

/// How often the app is redrawn at most for new log lines while the log is shown.
const LOG_REDRAW: std::time::Duration = std::time::Duration::from_millis(250);

/// Redraws the app for the server: its state at once; its log only while someone looks at it, a
/// few times a second at most (a busy server logs a line per sign-in, plan and change).
fn share_waker(repaint: Context, log_shown: std::sync::Arc<std::sync::atomic::AtomicBool>) -> plancraft_server::sharing::Waker {
    std::sync::Arc::new(move |wake| match wake {
        Wake::State => repaint.request_repaint(),
        Wake::Log if log_shown.load(std::sync::atomic::Ordering::Relaxed) => repaint.request_repaint_after(LOG_REDRAW),
        Wake::Log => {}
    })
}

impl ShareHost {
    /// A host that looks for a server kept sharing in the background in `config`.
    pub fn looking_in(config: Option<PathBuf>) -> Self {
        ShareHost { config, ..ShareHost::default() }
    }

    /// Looks at the servers. Returns what to tell, once: why sharing stopped when it stopped on
    /// its own, or why keeping it shared failed.
    pub fn poll(&mut self) -> Option<String> {
        if let Some(said) = self.poll_task() {
            return Some(said);
        }
        // Seen by whichever looked first (the status bar, the window, here).
        self.look(false);
        if let Some(said) = self.notice.take() {
            return Some(said);
        }
        let state = self.sharing.as_mut()?.state();
        if matches!(state, State::Starting | State::Stopping)
            && let Some(ctx) = &self.ctx
        {
            // Its end may be decided without news from the server (its last log lines never came).
            ctx.request_repaint_after(LOG_REDRAW);
        }
        match state {
            State::Stopped(why) => {
                self.last_log = self.sharing.take().map(|s| s.log()).unwrap_or_default();
                self.error = why.as_ref().map(|w| format!("Sharing stopped: {w}"));
                // Drawn now as stopped (the status bar may have drawn it as still stopping).
                if let Some(ctx) = &self.ctx {
                    ctx.request_repaint();
                }
                self.error.clone()
            }
            _ => None,
        }
    }

    /// Takes the answer of starting or stopping the background server, if it came.
    fn poll_task(&mut self) -> Option<String> {
        let (task, rx) = self.task.as_ref()?;
        let task = *task;
        let done = match rx.try_recv() {
            Ok(d) => d,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => Err("it stopped unexpectedly".into()),
        };
        // Looked at while the task still counts: a server it stopped did not stop on its own.
        self.look(true);
        self.task = None;
        if task == Task::Stop {
            self.stopping = None;
        }
        if let Some(ctx) = &self.ctx {
            ctx.request_repaint();
        }
        let said = done.err().map(|e| match task {
            Task::Start => {
                self.asked_start = None;
                format!("Could not keep sharing: {e}")
            }
            Task::Stop => format!("Stopping sharing: {e}"),
            Task::Forget => format!("Could not stop sharing at login: {e}"),
        });
        if task == Task::Start && std::mem::take(&mut self.stop_after_start) {
            self.stop_background();
        }
        if let Some(said) = &said {
            self.error = Some(said.clone());
        }
        said
    }

    /// Looks at the server kept sharing in the background (at most once a second, unless `now`).
    fn look(&mut self, now: bool) {
        let Some(config) = self.config.clone() else { return };
        if !now && self.seen.as_ref().is_some_and(|s| s.at.elapsed() < BACKGROUND_LOOK) {
            return;
        }
        let state = background::status(&config);
        let at_login = (self.login.enabled)();
        let has_log = std::fs::metadata(background::log_path(&config)).is_ok_and(|m| m.len() > 0);
        let log = (has_log && self.log_shown.load(std::sync::atomic::Ordering::Relaxed)).then(|| background::log_tail(&config, LOG_SHOWN));
        if matches!(state, Background::Sharing(_)) {
            self.asked_start = None;
        }
        let mut said = None;
        if let Some((at, folder)) = &self.asked_start
            && state == Background::NotRunning
            && self.task.is_none()
            && at.elapsed() >= BACKGROUND_START_WAIT
        {
            let log = if has_log { " File › Share Folder shows its log, which may say why." } else { "" };
            said = Some(format!("Sharing {} in the background did not start.{log}", folder_name(folder)));
            self.asked_start = None;
        }
        // It was sharing, and nobody here stopped it.
        if let Some(Background::Sharing(before)) = self.seen.as_ref().map(|s| &s.state)
            && state == Background::NotRunning
            && self.task.is_none()
        {
            let name = folder_name(&before.folder);
            said = Some(if at_login {
                format!("Sharing {name} in the background stopped. It starts again at your next login; File › Share Folder shows its log.")
            } else {
                format!("Sharing {name} in the background stopped.")
            });
        }
        if let Some(said) = said {
            self.error = Some(said.clone());
            self.notice = Some(said);
        }
        self.seen = Some(Seen { at: std::time::Instant::now(), state, at_login, has_log, log });
    }

    /// Whether the server (running, starting or stopping) is the one kept sharing in the
    /// background.
    pub fn in_background(&mut self) -> bool {
        self.sharing.is_none() && self.background_status().is_some()
    }

    /// Whether the server running now is listed on the network.
    pub fn announced(&mut self) -> bool {
        if self.sharing.is_some() {
            return self.announced;
        }
        self.look(false);
        matches!(self.seen.as_ref().map(|s| &s.state), Some(Background::Sharing(s)) if s.announced)
    }

    /// Whether the system shares a folder at login.
    pub fn at_login(&mut self) -> bool {
        self.look(false);
        self.seen.as_ref().is_some_and(|s| s.at_login)
    }

    /// The server kept sharing in the background, if it runs, starts or stops.
    fn background_status(&mut self) -> Option<(PathBuf, State)> {
        self.look(false);
        let asked = self.asked_start.as_ref().map(|(_, f)| f.clone());
        let task = self.task.as_ref().map(|(t, _)| *t);
        match (task, self.seen.as_ref().map(|s| &s.state)) {
            (Some(Task::Stop), Some(Background::Sharing(s))) => Some((s.folder.clone(), State::Stopping)),
            (Some(Task::Stop), _) => Some((self.stopping.clone().or(asked).unwrap_or_default(), State::Stopping)),
            (Some(Task::Start), _) if self.stop_after_start => Some((asked.unwrap_or_default(), State::Stopping)),
            (_, Some(Background::Sharing(s))) => {
                Some((s.folder.clone(), State::Sharing(Ready { addr: s.addr, fingerprint: s.fingerprint.clone() })))
            }
            (Some(Task::Start), _) | (_, Some(Background::Starting)) => Some((asked.unwrap_or_default(), State::Starting)),
            _ => asked.map(|f| (f, State::Starting)),
        }
    }

    /// The folder being shared and where the server is.
    pub fn status(&mut self) -> Option<(PathBuf, State)> {
        if let Some(s) = self.sharing.as_mut() {
            let state = s.state();
            return Some((s.folder().to_path_buf(), state));
        }
        self.background_status()
    }

    /// Whether there is a server log to show.
    pub fn has_log(&mut self) -> bool {
        if let Some(s) = &self.sharing {
            return s.log_len() > 0;
        }
        self.look(false);
        !self.last_log.is_empty() || self.seen.as_ref().is_some_and(|s| s.has_log)
    }

    /// The server's log (the one kept sharing in the background, or the last one this
    /// PlanCraft ran), oldest first.
    pub fn log(&mut self) -> Vec<String> {
        if let Some(s) = &self.sharing {
            return s.log();
        }
        self.look(false);
        // Just unfolded: read now, not at the next look.
        if self.seen.as_ref().is_some_and(|s| s.has_log && s.log.is_none()) && self.log_shown.load(std::sync::atomic::Ordering::Relaxed) {
            self.look(true);
        }
        match self.seen.as_ref().and_then(|s| s.log.as_ref()) {
            Some(log) if !log.is_empty() && self.last_log.is_empty() => log.clone(),
            _ => self.last_log.clone(),
        }
    }

    /// Keeps sharing `folder` in the background, from every login on (the system starts it).
    pub fn start_background(&mut self, program: &Program, folder: &Path, config: &Path, port: u16, announce: bool, ctx: &Context) {
        if self.task.is_some() {
            self.error = Some("PlanCraft is still changing your login items: try again in a moment.".into());
            return;
        }
        self.ctx = Some(ctx.clone());
        self.error = None;
        self.last_log.clear();
        self.config = Some(config.to_path_buf());
        self.asked_start = Some((std::time::Instant::now(), folder.to_path_buf()));
        let enable = self.login.enable;
        let (program, folder, config) = (program.clone(), folder.to_path_buf(), config.to_path_buf());
        // Off the window's thread: the folder may be on a network that is slow to answer.
        self.run_task(Task::Start, ctx, move || enable(&login::Entry::new(&program, &folder, &config, port, announce)?));
    }

    /// Stops the server kept sharing in the background (every plan saved), and starting it at
    /// login.
    fn stop_background(&mut self) {
        let (Some(config), Some(ctx)) = (self.config.clone(), self.ctx.clone()) else { return };
        match self.task.as_ref().map(|(t, _)| *t) {
            // Still setting the login item: stopped once that is done.
            Some(Task::Start) => {
                self.stop_after_start = true;
                return;
            }
            Some(_) => return,
            None => {}
        }
        let prepare = server_program().ok().and_then(|p| p.prepare);
        // Asked to start a moment ago: it may not have taken its folder yet, and would clear a
        // stop request made before (it takes one left over to be stale).
        let asked_start = self.asked_start.take();
        let starting = asked_start.is_some();
        self.stopping = asked_start.map(|(_, f)| f).or_else(|| match self.seen.as_ref().map(|s| &s.state) {
            Some(Background::Sharing(s)) => Some(s.folder.clone()),
            _ => None,
        });
        // Asked now (one small file), so it stops even if PlanCraft quits at once.
        let asked = !starting && background::status(&config) != Background::NotRunning && background::request_stop(&config).is_ok();
        let disable = self.login.disable;
        self.run_task(Task::Stop, &ctx, move || {
            // Not at login any more, whatever happens next; on macOS only once it has stopped,
            // as taking a LaunchAgent away signals the server (launchd waits only so long).
            let early = (!cfg!(target_os = "macos")).then(|| disable(prepare));
            if starting {
                let deadline = std::time::Instant::now() + BACKGROUND_START_WAIT;
                while background::status(&config) == Background::NotRunning && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
            let mut stopped = Ok(());
            if background::status(&config) != Background::NotRunning {
                if !asked && let Err(e) = background::request_stop(&config) {
                    stopped = Err(e);
                } else if !background::wait_stopped(&config, std::time::Duration::from_secs(60)) {
                    stopped = Err("it is still saving its plans after a minute; it stops once they are saved".into());
                }
            }
            let forgotten = early.unwrap_or_else(|| disable(prepare));
            stopped.and(forgotten)
        });
    }

    /// Waits up to `limit` for a login item being set or taken away (PlanCraft is quitting).
    pub fn finish_task(&mut self, limit: std::time::Duration) {
        if let Some((_, rx)) = &self.task {
            let _ = rx.recv_timeout(limit);
        }
    }

    /// Stops sharing a folder at login (none is shared now).
    pub fn forget_login(&mut self, ctx: &Context) {
        if self.task.is_some() {
            return;
        }
        self.ctx = Some(ctx.clone());
        self.error = None;
        let prepare = server_program().ok().and_then(|p| p.prepare);
        let disable = self.login.disable;
        self.run_task(Task::Forget, ctx, move || disable(prepare));
    }

    fn run_task(&mut self, task: Task, ctx: &Context, f: impl FnOnce() -> Result<(), String> + Send + 'static) {
        let (tx, rx) = mpsc::channel();
        let repaint = ctx.clone();
        let started = std::thread::Builder::new().name("plancraft-share-background".into()).spawn(move || {
            let done = plancraft_core::guard::catch("keeping a folder shared", f)
                .unwrap_or_else(|p| Err(format!("PlanCraft hit an internal error ({})", p.message)));
            let _ = tx.send(done);
            repaint.request_repaint();
        });
        match started {
            Ok(_) => self.task = Some((task, rx)),
            Err(e) => {
                self.asked_start = None;
                self.error = Some(format!("Could not keep sharing: {e}"));
            }
        }
    }

    /// Starts sharing `folder`.
    pub fn start(&mut self, program: &Program, folder: &Path, config: &Path, port: u16, announce: bool, ctx: &Context) {
        let repaint = ctx.clone();
        let log_shown = std::sync::Arc::clone(&self.log_shown);
        let waker = share_waker(repaint, log_shown);
        self.ctx = Some(ctx.clone());
        self.announced = announce;
        self.error = None;
        self.last_log.clear();
        match Sharing::start(program, folder, config, port, announce, waker) {
            Ok(s) => self.sharing = Some(s),
            Err(e) => self.error = Some(format!("Could not start sharing: {e}")),
        }
    }

    /// Whether someone is looking at the server's log (set every frame).
    pub fn watch_log(&self, shown: bool) {
        self.log_shown.store(shown, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether a new log line redraws the app now.
    #[cfg(test)]
    fn log_watched(&self) -> bool {
        self.log_shown.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The answer of a change to the users file made in a window since closed, once it came.
    pub fn take_answer(&mut self) -> Option<Result<String, String>> {
        let (_, rx) = self.pending.as_ref()?;
        let done = match rx.try_recv() {
            Ok(d) => d,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => Err("The change stopped unexpectedly.".into()),
        };
        self.pending = None;
        Some(done)
    }

    /// Asks the server to stop (it saves every plan first). The one kept sharing in the
    /// background is no longer started at login either.
    pub fn stop(&mut self) {
        if let Some(s) = self.sharing.as_mut() {
            s.stop();
        } else {
            self.stop_background();
        }
    }
}

/// `path` in at most `max` characters, its middle left out (the full path goes in a tooltip).
fn short_path(path: &Path, max: usize) -> String {
    let text = path.display().to_string();
    let n = text.chars().count();
    if n <= max {
        return text;
    }
    let head = max / 3;
    let tail = max.saturating_sub(head).saturating_sub(1);
    let start: String = text.chars().take(head).collect();
    let end: String = text.chars().skip(n.saturating_sub(tail)).collect();
    format!("{start}…{end}")
}

/// A folder's name, as shown.
pub fn folder_name(folder: &Path) -> String {
    if folder.as_os_str().is_empty() {
        // Not known yet (a server PlanCraft did not start, before it says what it shares).
        return "the shared folder".into();
    }
    folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| folder.display().to_string())
}

/// The program that runs the server: this PlanCraft as `plancraft serve`. From an AppImage, the
/// AppImage file itself (the executable sits in a temporary mount that goes away when this
/// PlanCraft quits, while the server is still saving its plans).
pub fn server_program() -> Result<Program, String> {
    let mut program = Program::this_plancraft()?;
    if let Some(image) = running_appimage(&program.program, std::env::var_os("APPIMAGE"), std::env::var_os("APPDIR")) {
        program.program = image;
    }
    // The server gets the environment PlanCraft was started with (not the bundled libraries).
    #[cfg(target_os = "linux")]
    {
        program.prepare = Some(crate::bundled_libs::clean_child);
    }
    Ok(program)
}

/// The AppImage file `exe` runs from, if any. The AppImage runtime names it in `APPIMAGE` and its
/// mount in `APPDIR`, but every program an AppImage starts inherits both: they count only when
/// `exe` sits inside that mount (PlanCraft started from another AppImage's terminal must not run
/// that AppImage).
pub fn running_appimage(exe: &Path, appimage: Option<std::ffi::OsString>, appdir: Option<std::ffi::OsString>) -> Option<PathBuf> {
    // AppImages are Linux only.
    if !cfg!(target_os = "linux") {
        return None;
    }
    let image = PathBuf::from(appimage.filter(|a| !a.is_empty())?);
    let dir = PathBuf::from(appdir.filter(|d| !d.is_empty())?);
    (image.is_absolute() && dir.is_absolute() && exe.starts_with(&dir)).then_some(image)
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

/// A change to the users file, made on a background thread. (No `Debug`: it holds passwords.)
#[derive(Clone)]
enum UserOp {
    Add { name: String, password: String, role: Role },
    Password { name: String, password: String },
    Role { name: String, role: Role, plan: Option<String> },
    ClearPlan { name: String, plan: String },
    Remove { name: String },
}

impl UserOp {
    /// What the change was typed into (cleared once it is made).
    fn form(&self) -> Form {
        match self {
            UserOp::Add { .. } => Form::Add,
            UserOp::Password { .. } => Form::Password,
            UserOp::Role { plan: Some(_), .. } => Form::Rule,
            UserOp::Remove { name } => Form::Removed(name.clone()),
            UserOp::Role { plan: None, .. } | UserOp::ClearPlan { .. } => Form::None,
        }
    }
}

/// The part of the window a change was typed into.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Form {
    None,
    Add,
    Password,
    Rule,
    /// Someone was removed: forms naming them go too.
    Removed(String),
}

/// Makes a change to the users file; returns what to say. New passwords are hashed first, then
/// the file is changed while no one else changes it (`Users::change`: another window of this
/// PlanCraft, a change still finishing after its window was closed, `plancraft serve user …`).
fn apply(path: &Path, op: UserOp) -> Result<String, String> {
    match op {
        UserOp::Add { name, password, role } => {
            let password = NewPassword::new(&password)?;
            Users::change(path, |u| u.add_with(&name, &password, role))?;
            Ok(format!("Added {} ({}).", name.trim(), role.name()))
        }
        UserOp::Password { name, password } => {
            let password = NewPassword::new(&password)?;
            Users::change(path, |u| u.set_password_with(&name, &password))?;
            Ok(format!("Changed the password of {name}."))
        }
        UserOp::Role { name, role, plan: None } => {
            Users::change(path, |u| u.set_role(&name, role, None))?;
            Ok(format!("{name} is now {}.", role.name()))
        }
        UserOp::Role { name, role, plan: Some(plan) } => {
            Users::change(path, |u| u.set_role(&name, role, Some(&plan)))?;
            Ok(format!("{name} is now {} for {plan}.", role.name()))
        }
        UserOp::ClearPlan { name, plan } => {
            Users::change(path, |u| u.clear_plan_role(&name, &plan))?;
            Ok(format!("{name} has the same access to {plan} as to the other plans again."))
        }
        UserOp::Remove { name } => {
            Users::change(path, |u| u.remove(&name))?;
            Ok(format!("Removed {name}."))
        }
    }
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

/// Where a change to the users file says how it went.
type Answer = mpsc::Receiver<Result<String, String>>;

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
    busy: Option<(String, Form, Answer)>,
    /// What the last change said (`Err`: it failed).
    note: Option<Result<String, String>>,
    /// The addresses shown for the server: its port, when they were looked up (again every few
    /// seconds: a laptop moves between networks), and them.
    addresses: Option<(u16, std::time::Instant, Vec<String>)>,
    /// The shared folder's plans open in this PlanCraft, and when that was looked at (once a
    /// second: it reads the disk).
    open_here: Option<(std::time::Instant, Vec<String>)>,
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
            open_here: None,
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
        let form = op.form();
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
                self.busy = Some((what.to_string(), form, rx));
            }
            Err(e) => self.note = Some(Err(format!("Cannot change who may sign in: {e}"))),
        }
    }

    /// Takes the answer of a change, if it has come.
    fn collect(&mut self) {
        let Some((_, form, rx)) = &self.busy else { return };
        let form = form.clone();
        let done = match rx.try_recv() {
            Ok(d) => d,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("The change stopped unexpectedly.".into()),
        };
        self.busy = None;
        if done.is_ok() {
            // Only the form the change came from: someone half-way through adding a person keeps
            // what they typed when they change another person's role meanwhile.
            match form {
                Form::Add => {
                    self.new_name.clear();
                    self.new_password.clear();
                }
                Form::Password => self.password_for = None,
                Form::Rule => self.new_rule = None,
                Form::Removed(name) => {
                    let gone = |n: &String| n.trim().eq_ignore_ascii_case(name.trim());
                    if self.password_for.as_ref().is_some_and(|(n, _)| gone(n)) {
                        self.password_for = None;
                    }
                    if self.new_rule.as_ref().is_some_and(|(n, _, _)| gone(n)) {
                        self.new_rule = None;
                    }
                }
                Form::None => {}
            }
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
        if self.busy.is_none()
            && let Some((what, rx)) = host.pending.take()
        {
            // A change made before the window was last closed, still going.
            self.busy = Some((what, Form::None, rx));
        }
        self.collect();
        self.reload_users();
        // To stop a server kept sharing in the background, started before this PlanCraft.
        host.ctx = Some(ctx.clone());
        // Changes from the command line show up without waiting for the mouse.
        ctx.request_repaint_after(std::time::Duration::from_secs(2));
        // Set again below while the log is unfolded.
        host.watch_log(false);
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
                // Taller than a small screen (many addresses, many people, the log): it scrolls.
                let max = (ctx.content_rect().height() - 160.0).max(200.0);
                egui::ScrollArea::vertical().id_salt("share_window_scroll").max_height(max).show(ui, |ui| {
                    if let Some(o) = self.server(ui, ctx, settings, host, open_here) {
                        outcome = o;
                    }
                    ui.add_space(6.0);
                    ui.separator();
                    let sharing_now = matches!(host.status(), Some((_, State::Sharing(_))));
                    self.people(ui, ctx, sharing_now);
                    let has_log = host.has_log();
                    if has_log {
                        ui.add_space(4.0);
                        egui::CollapsingHeader::new("Server log").id_salt("share_log").show(ui, |ui| {
                            // Unfolded: new lines redraw the app.
                            host.watch_log(true);
                            let log = host.log();
                            egui::ScrollArea::vertical().max_height(160.0).stick_to_bottom(true).show(ui, |ui| {
                                for line in log.iter().rev().take(200).rev() {
                                    ui.label(RichText::new(line).monospace().size(11.0));
                                }
                            });
                        });
                    }
                });
            });
        if !open {
            // A change still being made is told when it is done, window or not.
            host.pending = self.busy.take().map(|(what, _, rx)| (what, rx));
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
        let background = host.in_background();
        let at_login = background && host.at_login();
        let mut outcome = None;
        match status {
            Some((folder, state)) if !matches!(state, State::Stopped(_)) => {
                let name = folder_name(&folder);
                match &state {
                    State::Starting => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(format!("Starting to share {name}…"));
                            if ui.button("Cancel").clicked() {
                                host.stop();
                            }
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
                        if self.addresses.as_ref().is_none_or(|(p, at, _)| *p != port || at.elapsed() >= ADDRESSES_REFRESH) {
                            self.addresses = Some((port, std::time::Instant::now(), addresses_for(port)));
                            // Looked at again even while nothing else moves.
                            ctx.request_repaint_after(ADDRESSES_REFRESH);
                        }
                        let addresses = self.addresses.as_ref().map(|(_, _, a)| a.clone()).unwrap_or_default();
                        // Only this computer's name left (no address on any network).
                        if addresses.iter().all(|a| a.contains(".local")) {
                            ui.colored_label(theme::pal().WARN, "This computer is on no network right now.");
                        }
                        for a in &addresses {
                            copy_box(ui, a);
                        }
                        if host.announced() {
                            ui.label(
                                RichText::new("It is also listed under On your network in their Open from Server window.")
                                    .small()
                                    .color(theme::pal().TEXT_WEAK),
                            );
                        }
                        ui.add_space(4.0);
                        ui.label("On their first visit they check that the server's key fingerprint matches this one:");
                        copy_box(ui, &ready.fingerprint);
                        let stale = self.open_here.as_ref().is_none_or(|(at, _)| at.elapsed() >= std::time::Duration::from_secs(1));
                        if stale {
                            self.open_here = Some((std::time::Instant::now(), open_here(&folder)));
                            ctx.request_repaint_after(std::time::Duration::from_secs(1));
                        }
                        let here = self.open_here.as_ref().map(|(_, h)| h.clone()).unwrap_or_default();
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
                            let hover = if at_login {
                                "Saves every plan, tells everyone in them, and stops sharing at login"
                            } else {
                                "Saves every plan and tells everyone in them"
                            };
                            if ui.button("Stop Sharing").on_hover_text(hover).clicked() {
                                host.stop();
                            }
                        });
                        let note = match (background, at_login) {
                            (true, true) => "Sharing goes on when you quit PlanCraft, and starts again whenever you log in.",
                            (true, false) => "Sharing goes on when you quit PlanCraft.",
                            _ => "Sharing stops when you quit PlanCraft.",
                        };
                        ui.label(RichText::new(note).small().color(theme::pal().TEXT_WEAK));
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
                    // A long path would widen the window past the screen's edge.
                    Some(f) => ui.label(RichText::new(short_path(f, 56)).monospace()).on_hover_text(f.display().to_string()),
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
                let announce = ui.checkbox(&mut settings.share_announce, "List it for others on the network");
                if announce.changed() {
                    settings.save();
                }
                announce.on_hover_text(
                    "Everyone on the network this computer is on sees \"folder on computer\" in their Open from Server window, \
                     on any network it joins while sharing. People can still type the address with this off.",
                );
            });
            ui.end_row();
        });
        let keep = ui.checkbox(&mut settings.share_background, "Keep sharing after PlanCraft quits, and whenever you log in");
        if keep.changed() {
            settings.save();
        }
        keep.on_hover_text(KEEP_SHARING_HELP);
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
            ui.colored_label(theme::pal().ERROR, e.as_str());
        }
        if host.at_login() {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(theme::pal().WARN, "PlanCraft is set to share a folder whenever you log in, but it is not sharing now.");
                if ui.button("Don't Share at Login").clicked() {
                    host.forget_login(ctx);
                }
            });
        }
        ui.add_space(4.0);
        let ready = self.folder.is_some() && port.is_some() && self.users.as_ref().is_ok_and(|u| !u.is_empty());
        let start = egui::Button::new(RichText::new("Start Sharing").color(egui::Color32::WHITE)).fill(theme::pal().ACCENT);
        if ui.add_enabled(ready, start).clicked()
            && let (Some(folder), Some(port)) = (self.folder.clone(), port)
        {
            match server_program() {
                Ok(program) if settings.share_background => {
                    host.start_background(&program, &folder, &self.config, port, settings.share_announce, ctx);
                }
                Ok(program) => host.start(&program, &folder, &self.config, port, settings.share_announce, ctx),
                Err(e) => host.error = Some(e),
            }
            settings.share_folder = Some(folder);
            settings.share_port = port;
            settings.save();
        }
    }

    /// The bottom half: who may sign in.
    fn people(&mut self, ui: &mut egui::Ui, ctx: &Context, sharing: bool) {
        ui.label(RichText::new("People who may sign in").strong());
        let busy = self.busy.is_some();
        let users = match &self.users {
            Ok(u) => u.clone(),
            Err(e) => {
                ui.colored_label(theme::pal().ERROR, format!("Cannot read the list of people: {e}"));
                return;
            }
        };
        if users.is_empty() && sharing {
            ui.colored_label(theme::pal().WARN, "Nobody can sign in now: add someone below.");
        } else if users.is_empty() {
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
                            if let Some(f) = &self.folder {
                                self.plans = plans_in(f);
                            }
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
                ui.add_enabled_ui(!busy, |ui| {
                    if self.plans.is_empty() {
                        ui.add(egui::TextEdit::singleline(plan).hint_text("Plan.plancraft").desired_width(160.0));
                    } else {
                        egui::ComboBox::from_id_salt("share_rule_plan").selected_text(plan.as_str()).width(180.0).show_ui(ui, |ui| {
                            for p in &self.plans {
                                ui.selectable_value(plan, p.clone(), p);
                            }
                        });
                    }
                });
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
        if let Some((what, _, _)) = &self.busy {
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

/// What Keep sharing adds to the system, said where it is ticked.
const KEEP_SHARING_HELP: &str = if cfg!(target_os = "macos") {
    "The server runs on its own, so people can keep working while PlanCraft is closed. PlanCraft adds a login item for it \
     (System Settings › General › Login Items) and nothing else; Stop Sharing takes it away again."
} else if cfg!(windows) {
    "The server runs on its own, so people can keep working while PlanCraft is closed. PlanCraft adds PlanCraft Server to \
     the apps that start when you sign in (Task Manager › Startup apps) and nothing else; Stop Sharing takes it away again."
} else {
    "The server runs on its own, so people can keep working while PlanCraft is closed. PlanCraft adds a systemd user \
     service for it (plancraft-server.service), which runs while you are logged in, and nothing else; Stop Sharing takes it \
     away again."
};

/// How often the addresses shown are looked up again.
const ADDRESSES_REFRESH: std::time::Duration = std::time::Duration::from_secs(5);

/// The addresses people type to reach a server on `port` here: this computer's addresses, then
/// its name.
fn addresses_for(port: u16) -> Vec<String> {
    let mut list = plancraft_server::discovery::local_addresses(port);
    if let Some(name) = local_name() {
        list.push(if port == DEFAULT_PORT { name } else { format!("{name}:{port}") });
    }
    list
}

/// This computer's name on the local network (`NAME.local`), when it is one people can type.
fn local_name() -> Option<String> {
    local_name_of(&plancraft_server::server::host_name())
}

fn local_name_of(host: &str) -> Option<String> {
    let name = host.split('.').next().unwrap_or_default().trim();
    let typeable = !name.is_empty() && name.len() <= 63 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    typeable.then(|| format!("{name}.local"))
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

    /// Regression: `APPIMAGE` was trusted whenever it was set, so PlanCraft started from another
    /// AppImage's terminal ran that AppImage as its server.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_appimage_runs_the_server_only_when_plancraft_runs_from_it() {
        let mounted = Path::new("/tmp/.mount_PlanCrAbc/usr/bin/plancraft");
        let image = Some("/home/a/PlanCraft.AppImage".into());
        let dir = Some("/tmp/.mount_PlanCrAbc".into());
        assert_eq!(running_appimage(mounted, image.clone(), dir.clone()), Some(PathBuf::from("/home/a/PlanCraft.AppImage")));
        let installed = Path::new("/usr/bin/plancraft");
        assert_eq!(running_appimage(installed, image.clone(), dir.clone()), None, "inherited from another AppImage");
        assert_eq!(running_appimage(mounted, image.clone(), None), None);
        assert_eq!(running_appimage(mounted, None, dir.clone()), None);
        assert_eq!(running_appimage(mounted, Some("".into()), dir.clone()), None);
        assert_eq!(running_appimage(mounted, Some("PlanCraft.AppImage".into()), dir), None, "not a full path");
    }

    #[test]
    fn this_computer_is_offered_by_a_name_people_can_type() {
        assert_eq!(local_name_of("studio-pc").as_deref(), Some("studio-pc.local"));
        assert_eq!(local_name_of("Robs-Mac.local").as_deref(), Some("Robs-Mac.local"));
        assert_eq!(local_name_of("PlanCraft Server"), None);
        assert_eq!(local_name_of(""), None);
        assert_eq!(local_name_of("bür"), None);
    }

    /// Regression: every line the server logged redrew the whole app, window open or not.
    #[test]
    fn log_lines_redraw_only_while_the_log_is_read() {
        let ctx = Context::default();
        let shown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let wake = share_waker(ctx.clone(), std::sync::Arc::clone(&shown));
        // Frames until egui wants no more of its own (it redraws its first frames).
        let settle = || {
            for _ in 0..20 {
                ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
                if !ctx.has_requested_repaint() {
                    return;
                }
            }
            panic!("egui keeps asking for frames");
        };
        settle();
        wake(Wake::Log);
        assert!(!ctx.has_requested_repaint(), "nobody reads the log");
        wake(Wake::State);
        assert!(ctx.has_requested_repaint(), "the server's state always shows");
        settle();
        shown.store(true, std::sync::atomic::Ordering::Relaxed);
        wake(Wake::Log);
        assert!(ctx.has_requested_repaint(), "the log is unfolded");
    }

    /// Regression: a change wiped every half-filled form in the window, not just its own.
    #[test]
    fn a_change_clears_only_its_own_form() {
        let dir = crate::recovery::tests::TempDir::new("share-forms");
        let config = dir.0.as_path().join("server");
        let ctx = Context::default();
        let mut w = ShareFolder::new(&AppSettings::default(), config, None);
        w.change(&ctx, "", UserOp::Add { name: "Ana".into(), password: "correct horse".into(), role: Role::Editor });
        wait(&mut w);
        w.new_name = "Ben".into();
        w.new_password = "half typed".into();
        w.change(&ctx, "", UserOp::Role { name: "Ana".into(), role: Role::Viewer, plan: None });
        wait(&mut w);
        assert!(matches!(&w.note, Some(Ok(_))), "{:?}", w.note);
        assert_eq!((w.new_name.as_str(), w.new_password.as_str()), ("Ben", "half typed"));
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
        let host = ShareHost {
            sharing: None,
            error: Some("Sharing stopped: cannot listen on 0.0.0.0:47340: address in use".into()),
            last_log: vec!["plancraft-server: cannot listen on 0.0.0.0:47340: address in use".into()],
            ..ShareHost::default()
        };
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1000.0, 800.0)).build_ui_state(
            |ui, (w, settings, host): &mut (ShareFolder, AppSettings, ShareHost)| {
                let _ = w.show(ui.ctx(), settings, host, &|_| Vec::new());
            },
            (w, settings, host),
        );
        h.run();
        assert!(h.query_by_label_contains("Sharing stopped: cannot listen").is_some(), "why the last server stopped is shown");
        assert!(h.query_by_label_contains("Server log").is_some(), "its log is still there to read");
        // New log lines redraw the app only while someone reads the log.
        assert!(!h.state().2.log_watched(), "folded");
        h.get_by_label("Server log").click();
        h.run();
        assert!(h.state().2.log_watched(), "unfolded");
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

    /// Stands in for the system's login items: "starting at login" starts a server in this
    /// process that holds the private folder and stops when asked, as the real one does.
    static AT_LOGIN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    fn fake_enable(e: &login::Entry) -> Result<(), String> {
        AT_LOGIN.store(true, std::sync::atomic::Ordering::SeqCst);
        fake_server(e);
        Ok(())
    }

    /// A server started as the system would: it holds the private folder and stops when asked.
    fn fake_server(e: &login::Entry) {
        let config = e.config.clone();
        let folder = PathBuf::from(e.args.last().unwrap());
        assert!(e.args.iter().any(|a| a == "--background"));
        std::thread::spawn(move || {
            let claim = background::Claim::take(&config).unwrap().unwrap();
            let status = background::Status {
                folder,
                addr: "0.0.0.0:47399".parse().unwrap(),
                fingerprint: "aa bb cc".into(),
                version: "0.0.0".into(),
                announced: false,
            };
            claim.publish(&status).unwrap();
            while !claim.stop_requested() {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        });
    }

    fn fake_disable(_: Prepare) -> Result<(), String> {
        AT_LOGIN.store(false, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    fn fake_enabled() -> bool {
        AT_LOGIN.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Keep sharing: the server goes on in the background, the window shows where it is (also
    /// after PlanCraft restarts), and Stop Sharing stops it and takes it out of the login items.
    #[test]
    fn a_folder_kept_shared_runs_in_the_background_until_stopped() {
        use egui_kittest::kittest::Queryable;
        let dir = crate::recovery::tests::TempDir::new("share-background");
        let config = dir.0.join("server");
        let folder = dir.0.join("Team plans");
        std::fs::create_dir_all(&folder).unwrap();
        Users::change(&users_path(&config), |u| u.add_with("Ana", &NewPassword::new("correct horse").unwrap(), Role::Admin)).unwrap();
        let settings = AppSettings { share_folder: Some(folder.clone()), ..AppSettings::default() };
        let w = ShareFolder::new(&settings, config.clone(), None);
        let mut host = ShareHost::looking_in(Some(config.clone()));
        host.login = LoginItems { enable: fake_enable, disable: fake_disable, enabled: fake_enabled };
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1000.0, 1400.0)).build_ui_state(
            |ui, (w, settings, host): &mut (ShareFolder, AppSettings, ShareHost)| {
                let _ = w.show(ui.ctx(), settings, host, &|_| Vec::new());
                if let Some(said) = host.poll() {
                    panic!("{said}");
                }
            },
            (w, settings, host),
        );
        let until = |h: &mut egui_kittest::Harness<'_, (ShareFolder, AppSettings, ShareHost)>, what: &str| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while h.query_by_label_contains(what).is_none() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "never showed {what} (said: {:?}, seen {:?}, task {:?}, asked {:?}, keep {})",
                    h.state().2.error,
                    h.state().2.seen.as_ref().map(|s| s.state.clone()),
                    h.state().2.task.as_ref().map(|t| t.0),
                    h.state().2.asked_start,
                    h.state().1.share_background
                );
                std::thread::sleep(std::time::Duration::from_millis(50));
                h.step();
            }
        };
        h.run();
        h.get_by_label("Keep sharing after PlanCraft quits, and whenever you log in").click();
        h.run();
        assert!(h.state().1.share_background);
        h.get_by_label("Start Sharing").click();
        until(&mut h, "Sharing Team plans");
        until(&mut h, "starts again whenever you log in");
        assert!(fake_enabled());
        assert!(h.query_by_label_contains("aa bb cc").is_some(), "the fingerprint is the background server's");
        assert!(
            matches!(h.state_mut().2.status(), Some((f, State::Sharing(r))) if f == plancraft_core::io::canonical(&folder).unwrap() && r.addr.port() == 47399)
        );

        // PlanCraft quits and starts again: the window finds it.
        let mut again = ShareHost::looking_in(Some(config.clone()));
        again.login = h.state().2.login;
        assert!(again.in_background());
        h.state_mut().2 = again;
        h.run();
        until(&mut h, "Open a Plan from It");

        h.get_by_label("Stop Sharing").click();
        until(&mut h, "Start Sharing");
        assert!(!fake_enabled(), "no longer started at login");
        assert_eq!(background::status(&config), Background::NotRunning);
        assert!(h.query_by_label_contains("set to share a folder whenever you log in").is_none());

        // Set to start at login, but not running (it could not start, say): the window says so,
        // and takes it out of the login items when asked.
        AT_LOGIN.store(true, std::sync::atomic::Ordering::SeqCst);
        until(&mut h, "set to share a folder whenever you log in");
        h.get_by_label("Don't Share at Login").click();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while h.query_by_label_contains("set to share a folder whenever you log in").is_some() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(50));
            h.step();
        }
        assert!(!fake_enabled());
    }

    /// A server kept sharing in the background that stops on its own is told about once.
    #[test]
    fn a_background_server_that_stops_on_its_own_is_told() {
        let dir = crate::recovery::tests::TempDir::new("share-background-stops");
        let config = dir.0.join("server");
        let mut host = ShareHost::looking_in(Some(config.clone()));
        host.login = LoginItems { enable: fake_enable, disable: fake_disable, enabled: || false };
        let claim = background::Claim::take(&config).unwrap().unwrap();
        claim
            .publish(&background::Status {
                folder: dir.0.join("Plans"),
                addr: "0.0.0.0:47398".parse().unwrap(),
                fingerprint: "dd".into(),
                version: "0.0.0".into(),
                announced: true,
            })
            .unwrap();
        assert_eq!(host.poll(), None);
        assert!(host.in_background() && host.announced());
        drop(claim);
        // Looked at again a second later, first by the status bar (drawn before the poll).
        host.seen.as_mut().unwrap().at -= BACKGROUND_LOOK;
        assert!(host.status().is_none());
        assert_eq!(host.poll().as_deref(), Some("Sharing Plans in the background stopped."));
        assert_eq!(host.poll(), None);
        assert!(host.status().is_none());
    }

    /// The login item for the Cancel test: set only once RELEASED (a slow systemctl).
    static CANCEL_AT_LOGIN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    static RELEASED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    fn slow_enable(e: &login::Entry) -> Result<(), String> {
        while !RELEASED.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        CANCEL_AT_LOGIN.store(true, std::sync::atomic::Ordering::SeqCst);
        fake_server(e);
        Ok(())
    }

    /// Regression: Cancel did nothing while the login item was being set; now it stops sharing
    /// once that is done (and the login item is taken away again).
    #[test]
    fn cancel_while_the_login_item_is_set_stops_once_it_is() {
        let dir = crate::recovery::tests::TempDir::new("share-background-cancel");
        let config = dir.0.join("server");
        let folder = dir.0.join("Plans");
        std::fs::create_dir_all(&folder).unwrap();
        let mut host = ShareHost::looking_in(Some(config.clone()));
        host.login = LoginItems {
            enable: slow_enable,
            disable: |_| {
                CANCEL_AT_LOGIN.store(false, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
            enabled: || CANCEL_AT_LOGIN.load(std::sync::atomic::Ordering::SeqCst),
        };
        let program = Program { program: std::env::current_exe().unwrap(), args: vec!["serve".into()], prepare: None };
        let ctx = Context::default();
        host.start_background(&program, &folder, &config, 0, false, &ctx);
        assert!(matches!(host.status(), Some((_, State::Starting))));
        host.stop();
        assert!(matches!(host.status(), Some((ref f, State::Stopping)) if *f == folder), "{:?}", host.status());
        RELEASED.store(true, std::sync::atomic::Ordering::SeqCst);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while host.task.is_some() || host.status().is_some() {
            assert_eq!(host.poll(), None, "nothing went wrong, and it did not stop on its own");
            assert!(std::time::Instant::now() < deadline, "still {:?}", host.status());
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!CANCEL_AT_LOGIN.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(background::status(&config), Background::NotRunning);
        assert_eq!(host.error, None);
    }

    /// Regression: a long folder path widened the window past the screen's left edge, out of
    /// reach of the mouse (Start Sharing too).
    #[test]
    fn long_paths_are_shortened_in_the_middle() {
        let long = PathBuf::from(format!("/var/folders/{}/T/Team plans", "x".repeat(80)));
        let short = short_path(&long, 56);
        assert_eq!(short.chars().count(), 56);
        assert!(short.starts_with("/var/folders/") && short.ends_with("/T/Team plans") && short.contains('…'), "{short}");
        assert_eq!(short_path(Path::new("/srv/Plans"), 56), "/srv/Plans");
        assert_eq!(short_path(Path::new("/ääääääääää/ö"), 5).chars().count(), 5);
    }
}
