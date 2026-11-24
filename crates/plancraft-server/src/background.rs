//! A server kept sharing in the background: it goes on after PlanCraft quits and starts again
//! when its owner logs in ([`crate::login`]; docs/COLLABORATION.md › Phase 5).
//!
//! Such a server is `plancraft serve FOLDER --background` and keeps, in its private folder:
//!
//! - `background.lock`, held (locked) for as long as it runs: the system gives the lock back when
//!   the server ends, crashed or not, so a lock nobody holds means no server, whatever else lies
//!   about. One background server per private folder.
//! - `background.json`, what it shares and where, once it listens (the [`Status`]).
//! - `background.stop`, made by whoever wants it to stop (PlanCraft's *Stop Sharing*, or
//!   `plancraft serve stop`): it then saves every plan, tells everyone and ends. This works the
//!   same on every system, with no signals and no service manager, and only someone who may
//!   write to the private folder (its owner) can do it.
//! - `server.log` (and the one before, `server.log.1`), what it did, for PlanCraft's window.

use std::io::{Read, Seek, SeekFrom, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::Log;

const LOCK_FILE: &str = "background.lock";
const STATUS_FILE: &str = "background.json";
const STOP_FILE: &str = "background.stop";
const LOG_FILE: &str = "server.log";
const OLD_LOG_FILE: &str = "server.log.1";
/// A log file is set aside (as `server.log.1`, replacing the one before) once this large.
pub const MAX_LOG_BYTES: u64 = 1024 * 1024;
/// Longest status file read.
const MAX_STATUS_BYTES: u64 = 64 * 1024;
/// How long [`Claim::take`] tries for the lock: someone looking at the server
/// ([`status`]) holds it for a moment.
const CLAIM_WAIT: Duration = Duration::from_secs(3);
/// How often a background server looks for a stop request.
pub const STOP_POLL: Duration = Duration::from_millis(250);

/// What a background server shares, and where.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Status {
    pub folder: PathBuf,
    pub addr: SocketAddr,
    pub fingerprint: String,
    /// The PlanCraft version running it.
    pub version: String,
    /// It is listed on the network (mDNS).
    pub announced: bool,
}

/// Whether a background server runs from a private folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Background {
    NotRunning,
    /// Running, not listening yet (or it never said where).
    Starting,
    Sharing(Status),
}

fn lock_path(config: &Path) -> PathBuf {
    config.join(LOCK_FILE)
}

fn status_path(config: &Path) -> PathBuf {
    config.join(STATUS_FILE)
}

fn stop_path(config: &Path) -> PathBuf {
    config.join(STOP_FILE)
}

/// The server's log file.
pub fn log_path(config: &Path) -> PathBuf {
    config.join(LOG_FILE)
}

/// Opens (or makes, owner-only) a file the server alone writes in its private folder; never one
/// a link points to.
fn open_own(path: &Path) -> std::io::Result<std::fs::File> {
    let mut create = std::fs::OpenOptions::new();
    // `create_new` never follows a link: an existing file (or link) is left alone here.
    create.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        create.mode(0o600);
    }
    match create.open(path) {
        Ok(f) => Ok(f),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            plancraft_core::io::open_regular(path, true, plancraft_core::io::Links::Refuse)
        }
        Err(e) => Err(e),
    }
}

/// Removes a file, if there.
fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// The right to be the background server of a private folder, held while it runs.
pub struct Claim {
    config: PathBuf,
    /// Locked; closing it (also when the program ends) gives the lock back.
    _lock: std::fs::File,
}

impl std::fmt::Debug for Claim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Claim").field("config", &self.config).finish_non_exhaustive()
    }
}

impl Claim {
    /// Takes the claim on `config` (made if missing). `Ok(None)`: another background server
    /// holds it. A stop request or status left by a server that ended is cleared.
    pub fn take(config: &Path) -> Result<Option<Claim>, String> {
        plancraft_core::io::create_private_dir(config).map_err(|e| format!("cannot create {}: {e}", config.display()))?;
        let path = lock_path(config);
        let lock = open_own(&path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        let deadline = Instant::now() + CLAIM_WAIT;
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
                Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
                Err(std::fs::TryLockError::Error(e)) => return Err(format!("cannot lock {}: {e}", path.display())),
            }
        }
        // Whatever a server before this one left: a stop request must not stop this one.
        remove(&stop_path(config));
        remove(&status_path(config));
        Ok(Some(Claim { config: config.to_path_buf(), _lock: lock }))
    }

    /// Says what is shared and where ([`status`] gives it back).
    pub fn publish(&self, status: &Status) -> Result<(), String> {
        let text = serde_json::to_vec_pretty(status).map_err(|e| e.to_string())?;
        let path = status_path(&self.config);
        plancraft_core::io::write_atomic(&path, &text, plancraft_core::io::Privacy::OwnerOnly)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))
    }

    /// Whether someone asked the server to stop (the request is used up).
    pub fn stop_requested(&self) -> bool {
        take_stop_request(&self.config)
    }

    /// The private folder.
    pub fn config(&self) -> &Path {
        &self.config
    }
}

impl Drop for Claim {
    /// The status goes before the lock is given back, so a status is never read from a server
    /// that has ended.
    fn drop(&mut self) {
        remove(&status_path(&self.config));
        remove(&stop_path(&self.config));
    }
}

/// Uses up a stop request for the server of `config`, if there is one.
fn take_stop_request(config: &Path) -> bool {
    let path = stop_path(config);
    if std::fs::symlink_metadata(&path).is_err() {
        return false;
    }
    remove(&path);
    true
}

/// Whether a background server runs from `config`, and what it shares.
pub fn status(config: &Path) -> Background {
    let Ok(lock) = plancraft_core::io::open_regular(&lock_path(config), false, plancraft_core::io::Links::Refuse) else {
        return Background::NotRunning;
    };
    match lock.try_lock_shared() {
        // Nobody holds it: no server (the shared lock goes again as the file closes).
        Ok(()) => return Background::NotRunning,
        Err(std::fs::TryLockError::WouldBlock) => {}
        Err(std::fs::TryLockError::Error(_)) => return Background::NotRunning,
    }
    drop(lock);
    let Ok(file) = plancraft_core::io::open_regular(&status_path(config), false, plancraft_core::io::Links::Refuse) else {
        return Background::Starting;
    };
    let mut text = String::new();
    if file.take(MAX_STATUS_BYTES).read_to_string(&mut text).is_err() {
        return Background::Starting;
    }
    match serde_json::from_str::<Status>(&text) {
        Ok(s) => Background::Sharing(s),
        Err(_) => Background::Starting,
    }
}

/// Asks the background server of `config` to stop (it saves every plan first). Nothing happens
/// when none runs: the next one clears the request as it starts.
pub fn request_stop(config: &Path) -> Result<(), String> {
    let path = stop_path(config);
    open_own(&path).map(drop).map_err(|e| format!("cannot ask the server to stop ({}): {e}", path.display()))
}

/// Waits up to `limit` for the background server of `config` to end. Whether it has.
pub fn wait_stopped(config: &Path, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if status(config) == Background::NotRunning {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Watches for a stop request on a thread of its own; runs `stop` once one comes.
pub fn watch_stop(config: PathBuf, stop: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    std::thread::Builder::new().name("plancraft-server-stop-watch".into()).spawn(move || {
        while !take_stop_request(&config) {
            std::thread::sleep(STOP_POLL);
        }
        stop();
    })?;
    Ok(())
}

struct LogFile {
    path: PathBuf,
    file: Option<std::fs::File>,
    size: u64,
}

impl LogFile {
    fn open(path: &Path) -> LogFile {
        let file = open_own(path).ok();
        let size = file.as_ref().and_then(|f| f.metadata().ok()).map_or(0, |m| m.len());
        let mut log = LogFile { path: path.to_path_buf(), file, size };
        if let Some(f) = log.file.as_mut()
            && f.seek(SeekFrom::End(0)).is_err()
        {
            log.file = None;
        }
        log
    }

    fn write(&mut self, line: &str) {
        if self.size >= MAX_LOG_BYTES {
            // Set aside, replacing the one before: at most twice the limit on disk.
            self.file = None;
            let _ = std::fs::rename(&self.path, self.path.with_file_name(OLD_LOG_FILE));
            *self = LogFile::open(&self.path);
        }
        let Some(f) = self.file.as_mut() else { return };
        let bytes = line.len() as u64;
        if f.write_all(line.as_bytes()).is_ok() {
            self.size = self.size.saturating_add(bytes);
        }
    }
}

/// A log written to the server's log file in `config` (set aside once it is
/// [`MAX_LOG_BYTES`]), and passed on to `also`.
pub fn file_log(config: &Path, also: Log) -> Log {
    let file = Arc::new(Mutex::new(LogFile::open(&log_path(config))));
    Arc::new(move |line: &str| {
        let when = plancraft_core::dates::format("%Y-%m-%d %H:%M:%S", plancraft_core::dates::now());
        // One line, whatever the text holds (a folder's name may hold a line break).
        let clean: String = line.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        file.lock().unwrap_or_else(PoisonError::into_inner).write(&format!("{when}  {clean}\n"));
        also(line);
    })
}

/// The last `lines` lines of the server's log in `config` (at most the last 64 KiB of it).
pub fn log_tail(config: &Path, lines: usize) -> Vec<String> {
    const TAIL: u64 = 64 * 1024;
    let Ok(mut file) = plancraft_core::io::open_regular(&log_path(config), false, plancraft_core::io::Links::Refuse) else {
        return Vec::new();
    };
    let len = file.metadata().map_or(0, |m| m.len());
    let from = len.saturating_sub(TAIL);
    if file.seek(SeekFrom::Start(from)).is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if file.take(TAIL).read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut all: Vec<String> = text
        .split('\n')
        // Started part-way through a line: that part is dropped.
        .skip(usize::from(from > 0))
        .map(|l| l.chars().filter(|c| !c.is_control() || *c == '\t').take(1_000).collect::<String>())
        .collect();
    if all.last().is_some_and(String::is_empty) {
        all.pop();
    }
    let skip = all.len().saturating_sub(lines);
    all.split_off(skip)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    fn status_of(folder: &str) -> Status {
        Status {
            folder: PathBuf::from(folder),
            addr: "0.0.0.0:47340".parse().unwrap(),
            fingerprint: "ab cd".into(),
            version: "0.0.0".into(),
            announced: true,
        }
    }

    #[test]
    fn a_claim_is_held_until_dropped_and_says_what_it_shares() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("server");
        assert_eq!(status(&config), Background::NotRunning);
        let claim = Claim::take(&config).unwrap().unwrap();
        assert_eq!(status(&config), Background::Starting);
        // A second server is refused while the first runs.
        assert!(Claim::take(&config).unwrap().is_none());
        claim.publish(&status_of("/plans")).unwrap();
        assert_eq!(status(&config), Background::Sharing(status_of("/plans")));
        drop(claim);
        assert_eq!(status(&config), Background::NotRunning);
        assert!(!status_path(&config).exists());
        // Taken again once given back.
        assert!(Claim::take(&config).unwrap().is_some());
    }

    #[test]
    fn a_status_left_by_a_server_that_ended_is_not_believed() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().to_path_buf();
        // As if a server had crashed: its status is there, its lock is not held.
        std::fs::write(lock_path(&config), b"").unwrap();
        std::fs::write(status_path(&config), serde_json::to_vec(&status_of("/old")).unwrap()).unwrap();
        assert_eq!(status(&config), Background::NotRunning);
        // A stop request left over does not stop the next server.
        request_stop(&config).unwrap();
        let claim = Claim::take(&config).unwrap().unwrap();
        assert!(!claim.stop_requested());
        assert_eq!(status(&config), Background::Starting);
    }

    #[test]
    fn a_stop_request_is_seen_once() {
        let dir = tempfile::tempdir().unwrap();
        let claim = Claim::take(dir.path()).unwrap().unwrap();
        assert!(!claim.stop_requested());
        request_stop(dir.path()).unwrap();
        assert!(claim.stop_requested());
        assert!(!claim.stop_requested());
        // Watched on a thread of its own.
        let (tx, rx) = std::sync::mpsc::channel();
        watch_stop(dir.path().to_path_buf(), move || tx.send(()).unwrap()).unwrap();
        assert!(rx.recv_timeout(STOP_POLL * 3).is_err());
        request_stop(dir.path()).unwrap();
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn someone_looking_does_not_keep_a_server_from_starting() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().to_path_buf();
        std::fs::write(lock_path(&config), b"").unwrap();
        // Someone holds the lock to look, as `status` does, for a moment.
        let looker = std::fs::File::open(lock_path(&config)).unwrap();
        looker.try_lock_shared().unwrap();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(looker);
        });
        assert!(Claim::take(&config).unwrap().is_some());
        t.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn links_in_the_private_folder_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = dir.path().join("elsewhere");
        std::fs::write(&elsewhere, b"keep").unwrap();
        let config = dir.path().join("server");
        std::fs::create_dir(&config).unwrap();
        std::os::unix::fs::symlink(&elsewhere, lock_path(&config)).unwrap();
        assert!(Claim::take(&config).is_err());
        std::os::unix::fs::symlink(&elsewhere, log_path(&config)).unwrap();
        let log = file_log(&config, crate::quiet());
        log("not written through the link");
        assert_eq!(std::fs::read(&elsewhere).unwrap(), b"keep");
        assert!(log_tail(&config, 10).is_empty());
    }

    #[test]
    fn the_log_is_set_aside_when_large_and_its_tail_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().to_path_buf();
        let log = file_log(&config, crate::quiet());
        log("first\nline");
        log("second");
        let tail = log_tail(&config, 10);
        assert_eq!(tail.len(), 2);
        assert!(tail[0].ends_with("  first line"), "{tail:?}");
        assert!(tail[1].ends_with("  second"));
        assert_eq!(log_tail(&config, 1).len(), 1);
        let long = "x".repeat(10_000);
        for _ in 0..250 {
            log(&long);
        }
        let size = std::fs::metadata(log_path(&config)).unwrap().len();
        let old = std::fs::metadata(config.join(OLD_LOG_FILE)).unwrap().len();
        assert!(size < MAX_LOG_BYTES && (MAX_LOG_BYTES..MAX_LOG_BYTES + 20_000).contains(&old), "{size} {old}");
        // The tail starts on a whole line.
        let tail = log_tail(&config, 1_000);
        assert!(tail.iter().all(|l| l.contains("  ")), "{:?}", tail.first());
        // A log written by another server before this one is added to.
        drop(log);
        let again = file_log(&config, crate::quiet());
        again("third");
        assert!(log_tail(&config, 1)[0].ends_with("  third"));
        assert!(std::fs::metadata(log_path(&config)).unwrap().len() > size);
    }
}
