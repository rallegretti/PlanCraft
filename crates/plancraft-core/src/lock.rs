//! Lock files: noticing that a plan is open in another PlanCraft (another window, the headless MCP
//! server, or someone else's computer on a shared folder) as soon as it is opened, rather than
//! when that copy saves.
//!
//! An open plan `plan.plancraft` holds a small lock file `.plan.plancraft.lock` next to it (hidden
//! on Windows too) saying who has it open: user, computer, process, PlanCraft instance and since
//! when. The lock is advisory: a plan open elsewhere still opens, with a warning, and saving there
//! is still protected by the "changed on disk" check.
//!
//! A running PlanCraft touches its lock files every [`REFRESH`]; a lock not touched for
//! [`STALE_AFTER`] is left over from a crash or a computer that went to sleep, and is taken over.
//! On Linux a lock from this computer whose process is gone is taken over at once.
//!
//! Lock files are read as hostile input: at most [`MAX_LOCK_BYTES`] are read, anything that is not
//! a regular file (a directory, a FIFO, a symbolic link) is never read or written, and the names in
//! it are cut short and stripped of control characters before they are shown.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::workspace::Workspace;

/// How often a running PlanCraft touches its lock files.
pub const REFRESH: Duration = Duration::from_secs(30);
/// A lock not touched for this long is stale (left by a crash, or a computer asleep).
pub const STALE_AFTER: Duration = Duration::from_secs(150);
/// Most bytes read from a lock file.
pub const MAX_LOCK_BYTES: u64 = 4096;
/// A lock not touched for this long by the clock is stale at once. One younger (but past
/// [`STALE_AFTER`]) is stale only once it went untouched while this PlanCraft watched for
/// [`WATCH`]: another computer's clock may run ahead, which makes a live lock look old.
const SURELY_STALE: Duration = Duration::from_secs(3600);
/// How long a lock must stay untouched under this PlanCraft's own clock to count as stale: more
/// than one [`REFRESH`], in which a live PlanCraft touches it.
const WATCH: Duration = Duration::from_secs(45);
/// Lock files watched, at most (the list is cleared beyond it).
const MAX_WATCHED: usize = 1024;
/// Longest name kept from a lock file.
const MAX_NAME_CHARS: usize = 80;

/// Who has a plan open, as written in its lock file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Owner {
    pub user: String,
    pub host: String,
    pub pid: u32,
    /// The PlanCraft instance (one per running process).
    pub instance: String,
    /// When the plan was opened there, local time (`2026-11-18 10:32`).
    pub opened: String,
    /// The program: "PlanCraft 0.6.0" or "PlanCraft MCP server 0.6.0".
    pub app: String,
}

impl Owner {
    /// The names cut short and stripped of control characters (they come from another program).
    fn sanitized(self) -> Self {
        let clean = |s: String| s.chars().filter(|c| !c.is_control()).take(MAX_NAME_CHARS).collect::<String>().trim().to_string();
        Owner {
            user: clean(self.user),
            host: clean(self.host),
            instance: clean(self.instance),
            opened: clean(self.opened),
            app: clean(self.app),
            ..self
        }
    }

    /// Who and where, for messages: "Ana on studio-pc since 2026-11-18 10:32", "you in another
    /// PlanCraft window on this computer since …".
    pub fn describe(&self) -> String {
        let me = identity();
        let here = !self.host.is_empty() && self.host == me.host;
        let who = match (self.user.is_empty(), here && self.user == me.user) {
            (_, true) => "you".to_string(),
            (true, _) => "someone".to_string(),
            (false, _) => self.user.clone(),
        };
        let app = if self.app.is_empty() { "PlanCraft" } else { self.app.as_str() };
        let place = if here {
            "on this computer".to_string()
        } else if self.host.is_empty() {
            "on another computer".to_string()
        } else {
            format!("on {}", self.host)
        };
        let since = if self.opened.is_empty() { String::new() } else { format!(" since {}", self.opened) };
        format!("{who} in {app} {place}{since}")
    }
}

/// Whether this PlanCraft holds a plan's lock.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum LockState {
    /// No lock: the plan has no file yet, or its folder can't hold a lock file (read-only).
    #[default]
    None,
    /// This PlanCraft holds the lock.
    Held,
    /// The plan is open in another PlanCraft, which holds the lock.
    OpenElsewhere(Owner),
}

impl LockState {
    pub fn elsewhere(&self) -> Option<&Owner> {
        match self {
            LockState::OpenElsewhere(o) => Some(o),
            _ => None,
        }
    }
}

/// This running PlanCraft.
struct Identity {
    user: String,
    host: String,
    pid: u32,
    instance: String,
}

fn identity() -> &'static Identity {
    static ME: OnceLock<Identity> = OnceLock::new();
    ME.get_or_init(|| {
        let user =
            ["USER", "USERNAME", "LOGNAME"].iter().find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty())).unwrap_or_default();
        let host = gethostname::gethostname().to_string_lossy().into_owned();
        let me = Owner { user, host, ..Owner::default() }.sanitized();
        Identity { user: me.user, host: me.host, pid: std::process::id(), instance: Uuid::new_v4().to_string() }
    })
}

static APP_NAME: OnceLock<String> = OnceLock::new();

/// Names the program in the lock files this process writes ("PlanCraft MCP server 0.6.0"). Set
/// once at start-up; the default is "PlanCraft".
pub fn set_app_name(name: impl Into<String>) {
    let _ = APP_NAME.set(name.into());
}

/// The lock file of the plan at `plan`: `.NAME.lock` in the same folder.
pub fn lock_path(plan: &Path) -> Option<PathBuf> {
    let name = plan.file_name()?;
    let mut lock = OsString::from(".");
    lock.push(name);
    lock.push(".lock");
    Some(plan.with_file_name(lock))
}

/// What is at a plan's lock file.
#[derive(Debug, PartialEq, Eq)]
enum Found {
    Missing,
    /// Something else sits where the lock goes (a folder, a link, a device): never touched, and
    /// taken for someone unknown having the plan open, so planting it cannot silence the warning.
    Planted,
    /// Unreadable: never touched.
    Unusable,
    Ours,
    /// Stale, with the file's time when it was judged (a touch since means it is in use).
    Stale(Owner, Option<SystemTime>),
    Live(Owner),
}

/// The owner written in a lock file (`Err`: missing, or not a regular file that can be read).
fn read_owner(lock: &Path) -> Result<Owner, ErrorKind> {
    let mut bytes = Vec::new();
    crate::io::open_regular(lock, false, crate::io::Links::Refuse)
        .and_then(|f| f.take(MAX_LOCK_BYTES).read_to_end(&mut bytes))
        .map_err(|e| e.kind())?;
    // Not a lock PlanCraft wrote (garbage, cut short): someone unknown, judged by its age.
    Ok(serde_json::from_slice::<Owner>(&bytes).unwrap_or_default().sanitized())
}

fn watched() -> &'static Mutex<HashMap<PathBuf, (SystemTime, Instant)>> {
    static WATCHED: OnceLock<Mutex<HashMap<PathBuf, (SystemTime, Instant)>>> = OnceLock::new();
    WATCHED.get_or_init(Mutex::default)
}

/// How long the lock file has kept the time `modified` while this PlanCraft watched it.
fn untouched_for(lock: &Path, modified: SystemTime) -> Duration {
    let mut seen = watched().lock().unwrap_or_else(PoisonError::into_inner);
    match seen.get(lock) {
        Some((m, since)) if *m == modified => since.elapsed(),
        _ => {
            if seen.len() >= MAX_WATCHED {
                seen.clear();
            }
            seen.insert(lock.to_path_buf(), (modified, Instant::now()));
            Duration::ZERO
        }
    }
}

fn inspect(lock: &Path) -> Found {
    let meta = match fs::symlink_metadata(lock) {
        Ok(m) => m,
        Err(e) if e.kind() == ErrorKind::NotFound => return Found::Missing,
        Err(_) => return Found::Unusable,
    };
    if !meta.is_file() {
        return Found::Planted;
    }
    let owner = match read_owner(lock) {
        Ok(o) => o,
        Err(ErrorKind::NotFound) => return Found::Missing,
        Err(_) => return Found::Unusable,
    };
    let me = identity();
    if !owner.instance.is_empty() && owner.instance == me.instance {
        return Found::Ours;
    }
    // A time in the future (clocks that differ between computers) counts as just touched.
    let modified = meta.modified().ok();
    let age = modified.and_then(|m| SystemTime::now().duration_since(m).ok()).unwrap_or_default();
    let old = age > STALE_AFTER && (age > SURELY_STALE || modified.is_some_and(|m| untouched_for(lock, m) >= WATCH));
    let gone_here = !owner.host.is_empty() && owner.host == me.host && (owner.pid == me.pid || !process_alive(owner.pid));
    if old || gone_here { Found::Stale(owner, modified) } else { Found::Live(owner) }
}

/// Takes the stale lock of `stale` away. Moved aside first (only one PlanCraft can move the file):
/// if what was moved is not that stale lock after all (another PlanCraft took it over and wrote
/// its own meanwhile), it is put back, never replacing a lock written since.
fn take_over(lock: &Path, stale: &Owner, modified: Option<SystemTime>) {
    take_over_with(lock, stale, modified, |from, to| fs::hard_link(from, to));
}

/// [`take_over`], putting a lock back with `link` (a hard link; tests stand in a failing one).
fn take_over_with(lock: &Path, stale: &Owner, modified: Option<SystemTime>, link: impl Fn(&Path, &Path) -> std::io::Result<()>) {
    let Some(name) = lock.file_name() else { return };
    let mut aside_name = name.to_os_string();
    aside_name.push(format!(".{}.old", identity().instance));
    let aside = lock.with_file_name(aside_name);
    if fs::rename(lock, &aside).is_err() {
        // Gone already (someone else took it over), or it cannot be moved: look again.
        return;
    }
    // Someone else's lock written since, or the stale owner's touched since (it woke up): put
    // back. A rename keeps the file's time.
    let touched = modified.is_some_and(|m| fs::symlink_metadata(&aside).ok().and_then(|a| a.modified().ok()) != Some(m));
    if read_owner(&aside).is_ok_and(|o| o != *stale) || touched {
        put_back(&aside, lock, link);
    }
    let _ = fs::remove_file(&aside);
}

/// Puts the lock moved to `aside` back at `lock`, never replacing a lock written there since: a
/// hard link, or where the folder has none (FAT, exFAT, some network shares) a new file with the
/// same contents.
fn put_back(aside: &Path, lock: &Path, link: impl Fn(&Path, &Path) -> std::io::Result<()>) {
    match link(aside, lock) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
        Err(_) => {
            let mut bytes = Vec::new();
            let read = crate::io::open_regular(aside, false, crate::io::Links::Refuse)
                .and_then(|f| f.take(MAX_LOCK_BYTES).read_to_end(&mut bytes));
            if read.is_err() {
                return;
            }
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                options.attributes(0x2); // hidden, like a lock written afresh
            }
            if let Ok(mut f) = options.open(lock)
                && f.write_all(&bytes).is_err()
            {
                drop(f);
                let _ = fs::remove_file(lock);
            }
        }
    }
}

/// Whether process `pid` runs on this computer (as far as can be told without calling the
/// operating system: only Linux shows its processes as files; elsewhere, assume it does).
fn process_alive(pid: u32) -> bool {
    if cfg!(target_os = "linux") && Path::new("/proc/self").exists() {
        return pid != 0 && Path::new("/proc").join(pid.to_string()).exists();
    }
    true
}

fn write_new(lock: &Path) -> std::io::Result<()> {
    let me = identity();
    let owner = Owner {
        user: me.user.clone(),
        host: me.host.clone(),
        pid: me.pid,
        instance: me.instance.clone(),
        opened: crate::dates::format("%Y-%m-%d %H:%M", crate::dates::now_minute()),
        app: APP_NAME.get().cloned().unwrap_or_else(|| "PlanCraft".into()),
    };
    let mut options = OpenOptions::new();
    // Never replaces anything: a lock someone else just wrote, or a link planted in its place.
    options.write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        options.attributes(FILE_ATTRIBUTE_HIDDEN);
    }
    let mut file = options.open(lock)?;
    let written = serde_json::to_vec_pretty(&owner).map_err(std::io::Error::other).and_then(|b| file.write_all(&b));
    if written.is_err() {
        drop(file);
        let _ = fs::remove_file(lock);
    }
    written
}

/// Marks our lock file as still in use.
fn touch(lock: &Path) {
    if let Ok(f) = crate::io::open_regular(lock, true, crate::io::Links::Refuse) {
        let _ = f.set_modified(SystemTime::now());
    }
}

/// Takes, keeps or takes over the lock of the plan at `plan` (whose file should exist): our own
/// lock is touched, a missing or stale one is written, and a live one held by another PlanCraft
/// is reported. Never fails: a folder that can't hold a lock file gives [`LockState::None`].
pub fn claim(plan: &Path) -> LockState {
    let Some(lock) = lock_path(plan) else { return LockState::None };
    for _ in 0..3 {
        match inspect(&lock) {
            Found::Ours => {
                touch(&lock);
                return LockState::Held;
            }
            Found::Live(owner) => return LockState::OpenElsewhere(owner),
            Found::Planted => return LockState::OpenElsewhere(Owner::default()),
            Found::Unusable => return LockState::None,
            Found::Missing => match write_new(&lock) {
                Ok(()) => return LockState::Held,
                // Another PlanCraft was quicker: look again.
                Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
                Err(_) => return LockState::None,
            },
            Found::Stale(owner, modified) => take_over(&lock, &owner, modified),
        }
    }
    LockState::None
}

/// Whether the plan at `plan` is open in another PlanCraft (reads the lock, writes nothing).
pub fn open_elsewhere(plan: &Path) -> Option<Owner> {
    match inspect(&lock_path(plan)?) {
        Found::Live(owner) => Some(owner),
        Found::Planted => Some(Owner::default()),
        _ => None,
    }
}

/// Removes the plan's lock file if this PlanCraft holds it.
pub fn release(plan: &Path) {
    if let Some(lock) = lock_path(plan)
        && inspect(&lock) == Found::Ours
    {
        let _ = fs::remove_file(lock);
    }
}

/// One round of lock upkeep for every open plan: touches the locks held, takes over locks that
/// went stale or were removed, and takes the lock of a plan that was open elsewhere once that
/// PlanCraft closed it. The files are handled without holding the workspace lock. Returns whether
/// any document's [`LockState`] changed.
pub fn maintain(ws: &Mutex<Workspace>) -> bool {
    let targets: Vec<(u64, PathBuf, LockState)> = {
        let ws = ws.lock().unwrap_or_else(PoisonError::into_inner);
        ws.docs.iter().filter_map(|d| Some((d.handle, d.path.clone()?, d.lock.clone()))).collect()
    };
    let found: Vec<(u64, PathBuf, LockState, LockState)> = targets
        .into_iter()
        .filter_map(|(handle, path, old)| {
            // A plan never saved, or deleted: keep a lock already held, take none for a missing file.
            if old != LockState::Held && !path.is_file() {
                return None;
            }
            let new = claim(&path);
            Some((handle, path, old, new))
        })
        .collect();
    let mut ws = ws.lock().unwrap_or_else(PoisonError::into_inner);
    let mut changed = false;
    for (handle, path, old, new) in found {
        match ws.index_of(handle).and_then(|i| ws.docs.get_mut(i)) {
            Some(doc) if doc.path.as_ref() == Some(&path) => {
                if doc.lock != new {
                    doc.lock = new;
                    changed = true;
                }
            }
            // Closed or saved elsewhere meanwhile: give back a lock taken for it just now.
            _ => {
                if new == LockState::Held && old != LockState::Held {
                    release(&path);
                }
            }
        }
    }
    changed
}

/// Starts a thread that keeps the workspace's locks ([`maintain`] every [`REFRESH`]) until the
/// workspace is dropped. `on_change` runs when a document's lock state changed (to redraw).
pub fn spawn_keeper(ws: Weak<Mutex<Workspace>>, on_change: impl Fn() + Send + 'static) {
    let spawned = std::thread::Builder::new().name("plancraft-locks".into()).spawn(move || {
        loop {
            // Short naps, so the thread ends soon after the workspace does.
            for _ in 0..REFRESH.as_secs() {
                std::thread::sleep(Duration::from_secs(1));
                if ws.strong_count() == 0 {
                    return;
                }
            }
            let Some(shared) = ws.upgrade() else { return };
            let changed = crate::guard::catch("keeping lock files", || maintain(&shared)).unwrap_or(false);
            drop(shared);
            if changed {
                on_change();
            }
        }
    });
    if let Err(e) = spawned {
        use std::io::Write;
        let _ = writeln!(std::io::stderr().lock(), "plancraft: could not start keeping lock files ({e})");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Project;

    fn plan(dir: &Path) -> PathBuf {
        let path = dir.join("plan.plancraft");
        crate::io::save(&Project::new("Plan", crate::dates::ymd(2026, 11, 18).unwrap()), &path).unwrap();
        path
    }

    fn other() -> Owner {
        Owner {
            user: "ana".into(),
            host: "studio-pc".into(),
            pid: 4242,
            instance: Uuid::new_v4().to_string(),
            opened: "2026-11-18 09:15".into(),
            app: "PlanCraft 0.6.0".into(),
        }
    }

    fn write_lock(plan: &Path, owner: &Owner) {
        fs::write(lock_path(plan).unwrap(), serde_json::to_vec(owner).unwrap()).unwrap();
    }

    fn age(plan: &Path, by: Duration) {
        let f = OpenOptions::new().write(true).open(lock_path(plan).unwrap()).unwrap();
        f.set_modified(SystemTime::now() - by).unwrap();
    }

    #[test]
    fn the_lock_file_sits_next_to_the_plan() {
        assert_eq!(lock_path(Path::new("/a/b/plan.plancraft")), Some(PathBuf::from("/a/b/.plan.plancraft.lock")));
        assert_eq!(lock_path(Path::new("/")), None);
    }

    #[test]
    fn claim_release_and_claim_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = plan(dir.path());
        assert_eq!(claim(&path), LockState::Held);
        let written: Owner = serde_json::from_slice(&fs::read(lock_path(&path).unwrap()).unwrap()).unwrap();
        assert_eq!(written.pid, std::process::id());
        assert_eq!(claim(&path), LockState::Held, "our own lock is kept");
        assert_eq!(open_elsewhere(&path), None);
        release(&path);
        assert!(!lock_path(&path).unwrap().exists());
        assert_eq!(claim(&path), LockState::Held);
    }

    #[test]
    fn a_live_lock_of_another_plancraft_is_reported_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = plan(dir.path());
        write_lock(&path, &other());
        assert_eq!(claim(&path), LockState::OpenElsewhere(other_named(&path)));
        assert!(open_elsewhere(&path).is_some());
        release(&path);
        assert!(lock_path(&path).unwrap().exists(), "never removes someone else's lock");
        assert_eq!(other_named(&path).describe(), "ana in PlanCraft 0.6.0 on studio-pc since 2026-11-18 09:15");
    }

    fn other_named(path: &Path) -> Owner {
        serde_json::from_slice::<Owner>(&fs::read(lock_path(path).unwrap()).unwrap()).unwrap()
    }

    #[test]
    fn a_stale_lock_is_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let path = plan(dir.path());
        write_lock(&path, &other());
        age(&path, SURELY_STALE + Duration::from_secs(5));
        assert_eq!(claim(&path), LockState::Held);
    }

    /// Regression: another computer's clock running ahead made a live lock look stale, and it was
    /// taken over on every round; the other PlanCraft never learnt of it.
    #[test]
    fn a_lock_that_only_looks_old_is_watched_before_it_is_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let path = plan(dir.path());
        let lock = lock_path(&path).unwrap();
        write_lock(&path, &other());
        age(&path, STALE_AFTER + Duration::from_secs(5));
        assert!(matches!(claim(&path), LockState::OpenElsewhere(_)), "first seen: perhaps only another clock");
        // Touched meanwhile (a live PlanCraft): still live, and watched afresh.
        age(&path, STALE_AFTER + Duration::from_secs(9));
        assert!(matches!(claim(&path), LockState::OpenElsewhere(_)));
        // Untouched for longer than a refresh under this clock: stale.
        let modified = fs::metadata(&lock).unwrap().modified().unwrap();
        let long_ago = Instant::now().checked_sub(WATCH + Duration::from_secs(1)).unwrap();
        watched().lock().unwrap().insert(lock.clone(), (modified, long_ago));
        assert_eq!(claim(&path), LockState::Held);
    }

    /// Regression: two PlanCraft taking over one stale lock at once could both end up "holding"
    /// it, one having deleted the other's fresh lock.
    #[test]
    fn taking_over_never_removes_a_lock_written_meanwhile() {
        let dir = tempfile::tempdir().unwrap();
        let path = plan(dir.path());
        let lock = lock_path(&path).unwrap();
        let stale = other();
        // Another PlanCraft already took the stale lock over and wrote its own.
        let fresh = Owner { user: "ben".into(), instance: Uuid::new_v4().to_string(), ..other() };
        write_lock(&path, &fresh);
        take_over(&lock, &stale, None);
        assert_eq!(other_named(&path), fresh, "the lock written meanwhile is back in place");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2, "nothing left aside");
        // Where the folder has no hard links, it is put back all the same.
        take_over_with(&lock, &stale, None, |_, _| Err(std::io::Error::other("no hard links here")));
        assert_eq!(other_named(&path), fresh, "put back without a hard link");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2, "nothing left aside");
        // The stale owner touched its lock since it was judged stale (it woke up): put back.
        write_lock(&path, &stale);
        let judged = SystemTime::now() - Duration::from_secs(3600);
        take_over(&lock, &stale, Some(judged));
        assert_eq!(other_named(&path), stale, "a lock touched since is in use");
        // The stale one itself is taken away.
        let modified = fs::metadata(&lock).unwrap().modified().ok();
        take_over(&lock, &stale, modified);
        assert!(!lock.exists());
    }

    #[test]
    fn a_lock_from_a_process_of_this_computer_that_ended_is_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let path = plan(dir.path());
        let me = identity();
        // Same computer, same process id, but another instance: a crashed PlanCraft whose id
        // this process got.
        write_lock(&path, &Owner { host: me.host.clone(), pid: me.pid, ..other() });
        assert_eq!(claim(&path), LockState::Held);
    }

    #[test]
    fn hostile_lock_files_never_block_or_break_opening() {
        let dir = tempfile::tempdir().unwrap();
        let path = plan(dir.path());
        let lock = lock_path(&path).unwrap();
        // Garbage: someone unknown, until it goes stale.
        fs::write(&lock, b"\xff\xfe{{{ not json").unwrap();
        let LockState::OpenElsewhere(owner) = claim(&path) else { panic!() };
        assert_eq!(owner.describe(), "someone in PlanCraft on another computer");
        // Long and full of control characters.
        let long = Owner { user: format!("a\u{1b}[31m{}", "x".repeat(10_000)), ..other() };
        fs::write(&lock, serde_json::to_vec(&long).unwrap()).unwrap();
        let LockState::OpenElsewhere(owner) = claim(&path) else { panic!() };
        assert!(owner.user.chars().count() <= MAX_NAME_CHARS && !owner.user.contains('\u{1b}'));
        // A directory where the lock file goes: never touched, and taken for someone unknown (it
        // must not silence the warning for everyone).
        fs::remove_file(&lock).unwrap();
        fs::create_dir(&lock).unwrap();
        assert!(matches!(claim(&path), LockState::OpenElsewhere(o) if o.describe().starts_with("someone")));
        release(&path);
        assert!(lock.is_dir());
    }

    fn lock_of(ws: &Mutex<Workspace>) -> LockState {
        ws.lock().unwrap().active_doc().unwrap().lock.clone()
    }

    #[test]
    fn opening_saving_closing_and_quitting_keep_the_lock_file_right() {
        let dir = tempfile::tempdir().unwrap();
        let path = plan(dir.path());
        let mut ws = Workspace::default();
        let h = ws.open(&path).unwrap();
        assert_eq!(ws.active_doc().unwrap().lock, LockState::Held);
        assert!(lock_path(&path).unwrap().is_file());

        // Save As moves the lock to the new file.
        let copy = dir.path().join("copy.plancraft");
        ws.active_doc_mut().unwrap().save_as(&copy).unwrap();
        assert!(!lock_path(&path).unwrap().exists());
        assert!(lock_path(&copy).unwrap().is_file());
        ws.close(h);
        assert!(!lock_path(&copy).unwrap().exists(), "closing gives the lock back");

        // Save As onto a plan open in another tab: closing either keeps the lock for the other.
        let mut ws2 = Workspace::default();
        let first = ws2.open(&path).unwrap();
        let other = ws2.add(Project::new("Other", crate::dates::ymd(2026, 11, 18).unwrap()), None);
        ws2.active = ws2.index_of(other);
        ws2.active_doc_mut().unwrap().save_as(&path).unwrap();
        ws2.close(first);
        assert!(lock_path(&path).unwrap().is_file(), "the plan is still open in the other tab");
        assert_eq!(ws2.active_doc().unwrap().lock, LockState::Held);
        ws2.close(other);
        assert!(!lock_path(&path).unwrap().exists());

        // The same through another spelling of the path, and a Save As away from the shared file:
        // the other tab keeps its lock.
        let mut ws3 = Workspace::default();
        let first = ws3.open(&path).unwrap();
        let other = ws3.add(Project::new("Other", crate::dates::ymd(2026, 11, 18).unwrap()), None);
        ws3.active = ws3.index_of(other);
        let spelled = dir.path().join(".").join(path.file_name().unwrap());
        ws3.active_doc_mut().unwrap().save_as(&spelled).unwrap();
        ws3.active_doc_mut().unwrap().save_as(dir.path().join("away.plancraft")).unwrap();
        ws3.keep_locks();
        assert!(lock_path(&path).unwrap().is_file(), "the first tab still has the plan open");
        ws3.active = ws3.index_of(first);
        let again = ws3.add(Project::new("Again", crate::dates::ymd(2026, 11, 18).unwrap()), None);
        ws3.active = ws3.index_of(again);
        ws3.active_doc_mut().unwrap().save_as(&spelled).unwrap();
        ws3.close(first);
        assert!(lock_path(&path).unwrap().is_file(), "handed over through another spelling");
        drop(ws3);

        // Quitting (dropping the workspace) gives every lock back.
        let mut ws = Workspace::default();
        ws.open(&path).unwrap();
        drop(ws);
        assert!(!lock_path(&path).unwrap().exists());

        // A plan bound to a file not written yet is locked when first saved, not before.
        let mut ws = Workspace::default();
        let new = dir.path().join("new.plancraft");
        ws.add(Project::new("New", crate::dates::ymd(2026, 11, 18).unwrap()), Some(new.clone()));
        assert_eq!(ws.active_doc().unwrap().lock, LockState::None);
        assert!(!lock_path(&new).unwrap().exists());
        ws.active_doc_mut().unwrap().save().unwrap();
        assert_eq!(ws.active_doc().unwrap().lock, LockState::Held);
    }

    #[test]
    fn a_plan_open_elsewhere_opens_with_a_warning_and_takes_the_lock_once_free() {
        let dir = tempfile::tempdir().unwrap();
        let path = plan(dir.path());
        write_lock(&path, &other());
        let shared = Workspace::default().shared();
        let h = shared.lock().unwrap().open(&path).unwrap();
        assert!(matches!(lock_of(&shared), LockState::OpenElsewhere(o) if o.user == "ana"));
        assert!(!maintain(&shared), "still open there");

        // The other PlanCraft closed it: the lock is taken.
        fs::remove_file(lock_path(&path).unwrap()).unwrap();
        assert!(maintain(&shared));
        assert_eq!(lock_of(&shared), LockState::Held);

        // Kept fresh while held.
        age(&path, Duration::from_secs(100));
        assert!(!maintain(&shared));
        let modified = fs::metadata(lock_path(&path).unwrap()).unwrap().modified().unwrap();
        assert!(SystemTime::now().duration_since(modified).unwrap_or_default() < Duration::from_secs(60));

        // Taken over by another PlanCraft (this one looked stale to it, say after a sleep).
        write_lock(&path, &other());
        assert!(maintain(&shared));
        assert!(lock_of(&shared).elsewhere().is_some());
        shared.lock().unwrap().close(h);
        assert!(lock_path(&path).unwrap().exists(), "their lock stays");
    }

    #[cfg(unix)]
    #[test]
    fn a_link_in_place_of_the_lock_is_never_followed() {
        let dir = tempfile::tempdir().unwrap();
        let path = plan(dir.path());
        let target = dir.path().join("precious.txt");
        fs::write(&target, "keep me").unwrap();
        std::os::unix::fs::symlink(&target, lock_path(&path).unwrap()).unwrap();
        assert!(matches!(claim(&path), LockState::OpenElsewhere(_)), "a planted link warns");
        assert_eq!(fs::read_to_string(&target).unwrap(), "keep me");
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_folder_gives_no_lock() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = plan(dir.path());
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();
        let state = claim(&path);
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        // Root may write anyway.
        assert!(matches!(state, LockState::None | LockState::Held), "{state:?}");
    }
}
