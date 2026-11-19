//! A server run as a separate program for PlanCraft's *File › Share Folder…*.
//!
//! The desktop app runs `plancraft serve FOLDER --watch-stdin` rather than a server inside its own
//! process:
//!
//! - the server has its own lock identity, so a plan open in one of the app's windows is refused
//!   by the server ("open elsewhere") and a plan the server shares warns in the app, exactly as
//!   between two computers;
//! - a crash in one never takes the other down with it;
//! - closing the server's standard input stops it cleanly (every plan saved, everyone told), and
//!   that happens by itself if the app quits or crashes.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};

use crate::cli::Ready;

/// Log lines kept for the window.
pub const MAX_LOG_LINES: usize = 500;
/// Longest log line kept.
const MAX_LINE_CHARS: usize = 1_000;
/// Longest line read from the server (longer lines are cut).
const MAX_LINE_BYTES: u64 = 64 * 1024;

/// What changed, for the [`Waker`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    /// The server is ready, or has ended: [`Sharing::state`] may say something new.
    State,
    /// A line was added to the server's log (only worth drawing while someone looks at it).
    Log,
}

/// Wakes the app when something changed.
pub type Waker = Arc<dyn Fn(Wake) + Send + Sync>;

/// Where the server is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Starting,
    Sharing(Ready),
    /// Asked to stop: saving its plans.
    Stopping,
    /// Stopped; with why, when it stopped on its own (it could not start, or it crashed).
    Stopped(Option<String>),
}

#[derive(Default)]
struct Inner {
    ready: Option<Ready>,
    log: VecDeque<String>,
    /// The server's standard error has ended: every line it wrote is in `log`.
    log_done: bool,
}

/// How long [`Sharing::state`] waits, after the program ended, for the last of its log (its
/// last words say why it stopped) before giving up on it.
const LAST_WORDS_WAIT: std::time::Duration = std::time::Duration::from_secs(2);
/// What the server's command line puts before an error it stops on.
const FATAL_PREFIX: &str = "plancraft-server: ";

fn lock(m: &Mutex<Inner>) -> std::sync::MutexGuard<'_, Inner> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How to start the server program: `plancraft serve` (`program` = PlanCraft, `args` = `serve`)
/// or `plancraft-server`.
#[derive(Clone, Debug)]
pub struct Program {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    /// Adjusts the command before it starts (the app gives back the environment it was started
    /// with).
    pub prepare: Option<fn(&mut Command)>,
}

impl Program {
    /// This PlanCraft's own program, as `plancraft serve`.
    pub fn this_plancraft() -> Result<Program, String> {
        let program = std::env::current_exe().map_err(|e| format!("cannot find PlanCraft's own program: {e}"))?;
        Ok(Program { program: replaced_exe(program), args: vec!["serve".into()], prepare: None })
    }
}

/// Linux names a program replaced while it runs (a package upgrade) `… (deleted)`: the file now
/// at the original name is the one to start.
fn replaced_exe(program: PathBuf) -> PathBuf {
    let Some(name) = program.to_str().and_then(|p| p.strip_suffix(" (deleted)")) else { return program };
    let original = PathBuf::from(name);
    if original.is_file() { original } else { program }
}

/// A folder being shared by a server program this PlanCraft started.
pub struct Sharing {
    folder: PathBuf,
    /// Taken by the thread that reaps it when this is dropped.
    child: Option<Child>,
    /// Dropped to stop the server.
    stdin: Option<ChildStdin>,
    inner: Arc<Mutex<Inner>>,
    /// Set once the program has ended, with what to say about it.
    ended: Option<Option<String>>,
    /// The program has ended (when that was seen, and how), but its last log lines may still be
    /// on their way.
    exited: Option<(std::time::Instant, std::process::ExitStatus)>,
}

impl std::fmt::Debug for Sharing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sharing").field("folder", &self.folder).field("pid", &self.child.as_ref().map(Child::id)).finish_non_exhaustive()
    }
}

/// Reads a line of at most [`MAX_LINE_BYTES`] (the rest of a longer line is dropped). `None` at
/// the end.
fn read_line(r: &mut impl BufRead) -> Option<String> {
    let mut buf = Vec::new();
    match r.by_ref().take(MAX_LINE_BYTES).read_until(b'\n', &mut buf) {
        Ok(0) | Err(_) => return None,
        Ok(_) => {}
    }
    if buf.last() != Some(&b'\n') {
        skip_line(r);
    }
    let text = String::from_utf8_lossy(&buf);
    Some(text.trim_end_matches(['\r', '\n']).chars().filter(|c| !c.is_control() || *c == '\t').take(MAX_LINE_CHARS).collect())
}

/// Skips to the end of the line without keeping what it skips.
fn skip_line(r: &mut impl BufRead) {
    loop {
        let (done, used) = match r.fill_buf() {
            Ok([]) | Err(_) => return,
            Ok(chunk) => match chunk.iter().position(|b| *b == b'\n') {
                Some(at) => (true, at.saturating_add(1)),
                None => (false, chunk.len()),
            },
        };
        r.consume(used);
        if done {
            return;
        }
    }
}

fn spawn_reader(name: &str, f: impl FnOnce() + Send + 'static) -> Result<(), String> {
    std::thread::Builder::new().name(name.into()).spawn(f).map(|_| ()).map_err(|e| format!("cannot start the server: {e}"))
}

impl Sharing {
    /// Starts sharing `folder` on `port` (0: any free port) with the server's private files in
    /// `config`, announcing it on the network unless `announce` is false.
    pub fn start(program: &Program, folder: &Path, config: &Path, port: u16, announce: bool, waker: Waker) -> Result<Sharing, String> {
        let mut cmd = Command::new(&program.program);
        cmd.args(&program.args)
            .arg("--config")
            .arg(config)
            .arg("--port")
            .arg(port.to_string())
            .arg("--watch-stdin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if !announce {
            cmd.arg("--no-announce");
        }
        // `--`: a folder whose name starts with "-" is still the folder.
        cmd.arg("--").arg(folder);
        // Not wherever PlanCraft was started (a USB stick would stay in use while sharing).
        let _ = plancraft_core::io::create_private_dir(config);
        if config.is_dir() {
            cmd.current_dir(config);
        }
        if let Some(prepare) = program.prepare {
            prepare(&mut cmd);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW: no console window flashes up for the server.
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = cmd.spawn().map_err(|e| format!("cannot start the server ({}): {e}", program.program.display()))?;
        let inner = Arc::new(Mutex::new(Inner::default()));
        let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
        let started = (|| {
            if let Some(out) = stdout {
                let (inner, waker) = (Arc::clone(&inner), Arc::clone(&waker));
                spawn_reader("plancraft-share-stdout", move || {
                    let mut r = BufReader::new(out);
                    while let Some(line) = read_line(&mut r) {
                        if let Some(ready) = Ready::parse(&line) {
                            lock(&inner).ready = Some(ready);
                            waker(Wake::State);
                        }
                    }
                    waker(Wake::State);
                })?;
            }
            if let Some(err) = stderr {
                let (inner, waker) = (Arc::clone(&inner), Arc::clone(&waker));
                spawn_reader("plancraft-share-log", move || {
                    let mut r = BufReader::new(err);
                    while let Some(line) = read_line(&mut r) {
                        let mut i = lock(&inner);
                        i.log.push_back(line);
                        while i.log.len() > MAX_LOG_LINES {
                            i.log.pop_front();
                        }
                        drop(i);
                        waker(Wake::Log);
                    }
                    lock(&inner).log_done = true;
                    waker(Wake::State);
                })?;
            }
            Ok::<(), String>(())
        })();
        let stdin = child.stdin.take();
        let sharing = Sharing { folder: folder.to_path_buf(), child: Some(child), stdin, inner, ended: None, exited: None };
        // Dropping it stops the program again.
        started.map(|()| sharing)
    }

    /// The folder shared.
    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// Where the server is now.
    pub fn state(&mut self) -> State {
        if let Some(why) = &self.ended {
            return State::Stopped(why.clone());
        }
        let Some(child) = self.child.as_mut() else { return State::Stopped(None) };
        let exited = match self.exited {
            Some((_, status)) => Ok(Some(status)),
            None => child.try_wait(),
        };
        match exited {
            Ok(Some(status)) => {
                let seen = *self.exited.get_or_insert((std::time::Instant::now(), status));
                let (log_done, ready, last) = {
                    let i = lock(&self.inner);
                    (i.log_done, i.ready.is_some(), i.log.back().cloned().unwrap_or_default())
                };
                // Its last words may still be on their way (the log is read on another thread).
                if !log_done && seen.0.elapsed() < LAST_WORDS_WAIT {
                    return if self.stdin.is_none() || ready { State::Stopping } else { State::Starting };
                }
                let asked = self.stdin.is_none();
                let why = match last.strip_prefix(FATAL_PREFIX) {
                    // The server's own error (it could not start, or had to stop).
                    Some(error) if !status.success() => Some(error.to_string()),
                    // Stopped when asked: nothing to explain.
                    _ if asked && status.success() => None,
                    _ if status.success() => Some("The server was stopped from outside PlanCraft.".to_string()),
                    // Killed, or crashed: whatever it logged last is not why.
                    _ => Some(format!("The server stopped unexpectedly ({status}).")),
                };
                self.stdin = None;
                self.ended = Some(why.clone());
                State::Stopped(why)
            }
            Ok(None) if self.stdin.is_none() => State::Stopping,
            Ok(None) => {
                let i = lock(&self.inner);
                match i.ready.clone() {
                    // Its output has ended but it is not gone yet (it is ending: the system
                    // closes a program's output just before it can be waited for). Said as
                    // stopping, so the app looks again in a moment rather than waiting for news
                    // that already came.
                    Some(_) if i.log_done => State::Stopping,
                    Some(r) => State::Sharing(r),
                    None => State::Starting,
                }
            }
            Err(e) => {
                let why = Some(format!("Cannot tell whether the server is running: {e}"));
                self.ended = Some(why.clone());
                State::Stopped(why)
            }
        }
    }

    /// The server's log, oldest first.
    pub fn log(&self) -> Vec<String> {
        lock(&self.inner).log.iter().cloned().collect()
    }

    /// How many lines the server's log has.
    pub fn log_len(&self) -> usize {
        lock(&self.inner).log.len()
    }

    /// Asks the server to stop: it saves every plan, tells everyone and ends ([`State::Stopping`]
    /// until it has).
    pub fn stop(&mut self) {
        self.stdin = None;
    }
}

impl Drop for Sharing {
    /// Stops the server (it goes on saving its plans after this PlanCraft is gone) and reaps the
    /// program once it ends, on a thread of its own.
    fn drop(&mut self) {
        self.stdin = None;
        if let Some(mut child) = self.child.take()
            && !matches!(child.try_wait(), Ok(Some(_)))
        {
            let _ = std::thread::Builder::new().name("plancraft-share-reap".into()).spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replaced_program_is_started_from_its_name() {
        let exe = std::env::current_exe().unwrap();
        let deleted = PathBuf::from(format!("{} (deleted)", exe.display()));
        assert_eq!(replaced_exe(deleted), exe);
        let gone = PathBuf::from("/no/such/plancraft (deleted)");
        assert_eq!(replaced_exe(gone.clone()), gone);
        assert_eq!(replaced_exe(exe.clone()), exe);
    }

    #[test]
    fn long_and_odd_lines_are_cut() {
        let long = format!("{}\nnext\n", "x".repeat(200_000));
        let mut r = BufReader::new(long.as_bytes());
        assert_eq!(read_line(&mut r).unwrap().chars().count(), MAX_LINE_CHARS);
        assert_eq!(read_line(&mut r).as_deref(), Some("next"));
        assert_eq!(read_line(&mut r), None);
        // The skipped part is not kept, however long.
        let mut r = BufReader::with_capacity(16, std::io::repeat(b'y').take(10_000_000).chain(&b"\nlast\n"[..]));
        assert_eq!(read_line(&mut r).unwrap().chars().count(), MAX_LINE_CHARS);
        assert_eq!(read_line(&mut r).as_deref(), Some("last"));
        let mut r = BufReader::new(&b"a\x1b[2Jb\r\n\xff\xfe\n"[..]);
        assert_eq!(read_line(&mut r).as_deref(), Some("a[2Jb"));
        assert_eq!(read_line(&mut r).as_deref(), Some("\u{fffd}\u{fffd}"));
    }

    #[test]
    fn the_ready_line_reads_back() {
        let r = Ready { addr: "0.0.0.0:47340".parse().unwrap(), fingerprint: "ab cd".into() };
        assert_eq!(Ready::parse(&r.line()), Some(r));
        assert_eq!(Ready::parse("2026-11-19 10:00:00  Listening on 0.0.0.0:47340"), None);
        assert_eq!(Ready::parse("{\"ready\":{}}"), None);
    }
}
