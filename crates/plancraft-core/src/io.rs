//! Reading and writing `.plancraft` documents and resolving links between documents.
//!
//! Documents are pretty-printed JSON so they diff well under version control
//! and are easy for tools to inspect.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use uuid::Uuid;

use crate::ids::TaskId;
use crate::limits::{self, MAX_FILE_BYTES};
use crate::model::*;
use crate::schedule::recalculate;

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("I/O error on {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("{path} is not a valid PlanCraft document: {source}")]
    Parse { path: PathBuf, source: serde_json::Error },
    #[error("{0} is not a PlanCraft document")]
    WrongFormat(PathBuf),
    #[error("{path} was written by a newer PlanCraft (format version {version})")]
    TooNew { path: PathBuf, version: u32 },
    #[error("could not serialize project: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("{path} is too large to open ({size} bytes; the maximum is {max} bytes)", max = MAX_FILE_BYTES)]
    TooLarge { path: PathBuf, size: u64 },
    #[error("{path} cannot be opened: {message}")]
    Limit { path: PathBuf, message: String },
    #[error("{path} was changed by another program since it was opened or last saved")]
    ChangedOnDisk { path: PathBuf },
    #[error("{path} could not be imported: {message}")]
    Import { path: PathBuf, message: String },
    #[error("{path} now contains a different project ('{name}'), not the one this plan links to")]
    DifferentProject { path: PathBuf, name: String },
}

/// What a file looked like on disk (modification time and size), to notice when another program
/// rewrites a document between our load or save and the next save, and to watch open documents
/// for changes ([`crate::watch`]).
///
/// The file's identity (inode) is deliberately left out: FUSE file systems (sshfs) and some
/// network mounts make up inode numbers that change while the file does not, which would report
/// changes that never happened. A rewrite of the same size within one tick of a coarse clock
/// (FAT counts in two-second steps) can go unnoticed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskStamp {
    modified: Option<std::time::SystemTime>,
    len: u64,
}

/// The file's current stamp, or `None` when it does not exist or cannot be read.
pub fn disk_stamp(path: impl AsRef<Path>) -> Option<DiskStamp> {
    let m = std::fs::metadata(path).ok()?;
    Some(DiskStamp { modified: m.modified().ok(), len: m.len() })
}

pub type FileResult<T> = Result<T, FileError>;

/// Serializes a project to pretty JSON.
pub fn to_json(p: &Project) -> FileResult<String> {
    let mut s = serde_json::to_string_pretty(p)?;
    s.push('\n');
    Ok(s)
}

/// Parses a project from JSON text.
pub fn from_json(text: &str, path: &Path) -> FileResult<Project> {
    from_json_with_notes(text, path).map(|(p, _)| p)
}

/// The fields checked before the whole document is parsed.
#[derive(serde::Deserialize)]
struct Header {
    format: Option<String>,
    version: Option<u64>,
}

/// Parses a project from JSON text, treating it as untrusted.
///
/// Documents with more items than [`crate::limits`] allow are refused; out-of-range values are
/// clamped and described in the returned notes.
pub fn from_json_with_notes(text: &str, path: &Path) -> FileResult<(Project, Vec<String>)> {
    let header: Header = serde_json::from_str(text).map_err(|source| FileError::Parse { path: path.into(), source })?;
    if header.format.as_deref() != Some(FILE_FORMAT) {
        return Err(FileError::WrongFormat(path.into()));
    }
    let version = header.version.map_or(0, |v| u32::try_from(v).unwrap_or(u32::MAX));
    if version > FILE_VERSION {
        return Err(FileError::TooNew { path: path.into(), version });
    }
    let mut p: Project = match serde_json::from_str(text) {
        Ok(p) => p,
        // Dates are limited to years −9999…9999; a file with a date beyond that (only a damaged or
        // hostile one) is read with those dates pulled to the end of the range, and `sanitize`
        // below then clamps them into 1900–2300 with a note, instead of the plan being refused.
        Err(source) => {
            let mut value: serde_json::Value = serde_json::from_str(text).map_err(|e| FileError::Parse { path: path.into(), source: e })?;
            if !clamp_extended_years(&mut value) {
                return Err(FileError::Parse { path: path.into(), source });
            }
            serde_json::from_value(value).map_err(|source| FileError::Parse { path: path.into(), source })?
        }
    };
    limits::check_counts(&p).map_err(|e| FileError::Limit { path: path.into(), message: e.to_string() })?;
    p.version = FILE_VERSION;
    // Keep id counters ahead of every stored id, of every kind (hand-edited files may lag behind).
    crate::sync::fit_counters(&mut p);
    // Plans saved before the Task Board existed get its default columns, numbered from 1 (no
    // column exists yet, so whatever the stored counter says, these ids are free and in range).
    if p.board_columns.is_empty() {
        let mut fresh = crate::ids::IdCounters::default();
        p.board_columns = crate::board::default_columns(&mut fresh);
    }
    p.ids.board_column = p.ids.board_column.max(p.board_columns.iter().map(|c| c.id.0).max().unwrap_or(0));
    if p.calendars.is_empty() {
        let id = p.ids.next_calendar();
        p.calendars.push(crate::calendar::Calendar::standard(id));
        p.calendar = id;
    }
    let notes = limits::sanitize(&mut p);
    Ok((p, notes))
}

/// Rewrites the dates in `v` that `jiff` can't read but plan files may hold (`chrono` read them):
/// a signed year not written with six digits (`+2026-…`, `-0001-…`) is re-padded, and a year
/// beyond ±9999 becomes the last (or first) day `jiff` holds, keeping any time of day. Only a
/// string that is a whole date or date-time changes, never a name or note that starts like one.
/// Returns whether anything changed.
fn clamp_extended_years(v: &mut serde_json::Value) -> bool {
    match v {
        serde_json::Value::String(s) => match normalized_signed_date(s) {
            Some(fixed) => {
                *s = fixed;
                true
            }
            None => false,
        },
        serde_json::Value::Array(items) => items.iter_mut().fold(false, |changed, x| clamp_extended_years(x) | changed),
        serde_json::Value::Object(map) => map.values_mut().fold(false, |changed, x| clamp_extended_years(x) | changed),
        _ => false,
    }
}

/// See [`clamp_extended_years`]: the rewritten text, or `None` to leave `s` as it is.
fn normalized_signed_date(s: &str) -> Option<String> {
    let sign = s.chars().next().filter(|c| matches!(c, '+' | '-'))?;
    let digits = s.get(1..)?.bytes().take_while(u8::is_ascii_digit).count();
    let year_text = s.get(1..1 + digits)?;
    let month_day = s.get(1 + digits..digits + 7)?;
    let rest = s.get(digits + 7..)?;
    let date_shaped =
        digits > 0 && matches!(month_day.as_bytes(), [b'-', m1, m2, b'-', d1, d2] if [m1, m2, d1, d2].iter().all(|c| c.is_ascii_digit()));
    // A date alone, or with a time of day: nothing else may follow.
    let time_shaped =
        rest.is_empty() || rest.strip_prefix('T').is_some_and(|t| t.bytes().all(|c| c.is_ascii_digit() || c == b':' || c == b'.'));
    if !date_shaped || !time_shaped {
        return None;
    }
    let fixed = match year_text.parse::<u32>() {
        Ok(year) if year <= 9999 => format!("{sign}{year:06}{month_day}{rest}"),
        _ if sign == '+' => format!("9999-12-31{rest}"),
        _ => format!("-009999-01-01{rest}"),
    };
    (fixed != s).then_some(fixed)
}

/// How [`open_regular`] treats a symbolic link at the path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Links {
    /// Followed to the file it points at.
    Follow,
    /// Refused (Unix; elsewhere only checked before opening).
    Refuse,
}

/// Opens `path` for reading (or writing, without truncating) only if it is a regular file, and
/// checks the file actually opened, not the name: something swapped in after a check (a FIFO,
/// which would block the open forever, a device, a folder, or a link with [`Links::Refuse`]) is
/// refused instead of read. On Unix the open never blocks.
pub fn open_regular(path: &Path, write: bool, links: Links) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    if write {
        opts.write(true);
    } else {
        opts.read(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let nofollow = if links == Links::Refuse { libc::O_NOFOLLOW } else { 0 };
        opts.custom_flags(libc::O_NONBLOCK | nofollow);
    }
    #[cfg(not(unix))]
    if links == Links::Refuse && std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{} is a link", path.display())));
    }
    let file = opts.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{} is not a regular file", path.display())));
    }
    Ok(file)
}

/// Loads a project document and recalculates it.
pub fn load(path: impl AsRef<Path>) -> FileResult<Project> {
    load_with_notes(path).map(|(p, _)| p)
}

/// Loads a project document and recalculates it, also returning what was clamped into range.
pub fn load_with_notes(path: impl AsRef<Path>) -> FileResult<(Project, Vec<String>)> {
    let path = path.as_ref();
    let io_err = |source| FileError::Io { path: path.into(), source };
    // Only regular files: opening a FIFO or device blocks or never ends (a hostile link or
    // argument must not hang the app or the MCP server).
    if !std::fs::metadata(path).map_err(io_err)?.is_file() {
        return Err(io_err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file")));
    }
    let file = open_regular(path, false, Links::Follow).map_err(io_err)?;
    let size = file.metadata().map_err(io_err)?.len();
    if size > MAX_FILE_BYTES {
        return Err(FileError::TooLarge { path: path.into(), size });
    }
    let mut text = String::new();
    file.take(MAX_FILE_BYTES + 1).read_to_string(&mut text).map_err(io_err)?;
    let size = u64::try_from(text.len()).unwrap_or(u64::MAX);
    if size > MAX_FILE_BYTES {
        return Err(FileError::TooLarge { path: path.into(), size });
    }
    let (mut p, notes) = from_json_with_notes(&text, path)?;
    recalculate(&mut p);
    Ok((p, notes))
}

/// Saves a project document atomically (see [`write_atomic`]).
pub fn save(p: &Project, path: impl AsRef<Path>) -> FileResult<()> {
    save_stamped(p, path).map(|_| ())
}

/// [`save`], returning the stamp of the file as written (see [`write_atomic_stamped`]).
pub fn save_stamped(p: &Project, path: impl AsRef<Path>) -> FileResult<DiskStamp> {
    let path = path.as_ref();
    let text = to_json(p)?;
    write_atomic_stamped(path, text.as_bytes(), Privacy::KeepExisting).map_err(|source| FileError::Io { path: path.into(), source })
}

/// Who may read a file written by [`write_atomic`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Privacy {
    /// The user's own document (a plan, an export). A new file is readable by its owner only (0600
    /// on Unix); a replaced file keeps its permissions and (on Unix) its group, a symbolic link is
    /// followed so its target is updated, and a read-only file is refused rather than replaced.
    KeepExisting,
    /// Always readable by the owner only (settings, recovery copies, session tokens). A symbolic
    /// link at `path` is replaced, never followed, so private data can't be redirected elsewhere.
    OwnerOnly,
    /// A plan PlanCraft Server shares from a folder others can write to: like `KeepExisting`
    /// (permissions and group kept, read-only refused), but `path` must be a regular file or
    /// missing. A link is refused rather than followed, so nobody can point the server's writes
    /// at another file; one swapped in at the last moment is replaced by the rename, never
    /// written through.
    NoLinks,
}

/// Most symbolic links followed in a row before giving up (as Linux's `ELOOP` limit).
const MAX_SYMLINK_HOPS: usize = 40;

/// Where a write to `path` should land: `path` itself, or (when it is a symbolic link) the file it
/// finally points at, so that saving through a link updates the real file instead of replacing
/// the link with a copy. A link to a file that does not exist yet gives that file's path.
fn resolve_symlinks(path: &Path) -> std::io::Result<PathBuf> {
    let mut current = path.to_path_buf();
    for _ in 0..MAX_SYMLINK_HOPS {
        match std::fs::symlink_metadata(&current) {
            Ok(m) if m.file_type().is_symlink() => {
                let target = std::fs::read_link(&current)?;
                current = match current.parent() {
                    Some(dir) if target.is_relative() => dir.join(target),
                    _ => target,
                };
            }
            _ => return Ok(current),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{} is a loop of symbolic links", path.display())))
}

/// Writes `bytes` to `path` so that a crash or power cut never leaves a torn file.
///
/// The data goes to a temporary file in the same directory, is flushed to disk (`fsync`) and is
/// then renamed over `path` in one step, so `path` holds either the old or the new contents. On
/// Unix the directory is synced too, so the rename itself survives a power cut.
///
/// With [`Privacy::KeepExisting`] a symbolic link is followed (the temporary file goes next to
/// its target) and a read-only file is refused with [`std::io::ErrorKind::PermissionDenied`].
/// The replaced file's permissions and group are kept; its access control lists (ACLs) and other
/// extended attributes are not, as copying them needs platform code beyond the standard library.
pub fn write_atomic(path: &Path, bytes: &[u8], privacy: Privacy) -> std::io::Result<()> {
    write_atomic_stamped(path, bytes, privacy).map(|_| ())
}

/// [`write_atomic`], returning the stamp of the file written, taken from the new file itself just
/// before it took the name (a rename keeps the time and size). Reading it by name afterwards could
/// take another program's write, landing in between, for this one.
pub fn write_atomic_stamped(path: &Path, bytes: &[u8], privacy: Privacy) -> std::io::Result<DiskStamp> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let resolved;
    let path = match privacy {
        Privacy::KeepExisting => {
            resolved = resolve_symlinks(path)?;
            resolved.as_path()
        }
        Privacy::OwnerOnly => path,
        Privacy::NoLinks => match std::fs::symlink_metadata(path) {
            Ok(m) if !m.is_file() => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("{} is not a regular file (a link, a folder or a device); it is not written", path.display()),
                ));
            }
            _ => path,
        },
    };
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let name = path.file_name().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a file path"))?;
    let tmp = dir.join(format!(".{}.{}-{}.tmp", name.to_string_lossy(), std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed)));
    let existing = match privacy {
        Privacy::KeepExisting => std::fs::metadata(path).ok(),
        Privacy::NoLinks => std::fs::symlink_metadata(path).ok(),
        Privacy::OwnerOnly => None,
    };
    // Read-only by its permission bits, or not writable by this user for another reason (a file
    // owned by someone else, in a folder this user may write to, would otherwise be replaced).
    let not_writable = existing.as_ref().is_some_and(|m| {
        m.permissions().readonly()
            || matches!(open_regular(path, true, if privacy == Privacy::NoLinks { Links::Refuse } else { Links::Follow }), Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied)
    });
    if not_writable {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("{} is read-only; make it writable or save under another name", path.display()),
        ));
    }
    let result = (|| {
        let mut f = create_private(&tmp)?;
        f.write_all(bytes)?;
        if let Some(m) = &existing {
            // The group first (best effort: only a group the user belongs to can be set). If it
            // can't be kept, the old file's group permissions are not given to the user's own
            // group instead.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let kept = std::os::unix::fs::fchown(&f, None, Some(std::os::unix::fs::MetadataExt::gid(m))).is_ok();
                let mode = m.permissions().mode();
                f.set_permissions(std::fs::Permissions::from_mode(if kept { mode } else { mode & !0o070 }))?;
            }
            #[cfg(not(unix))]
            f.set_permissions(m.permissions())?;
        }
        f.sync_all()?;
        // Stamped once closed, by the temporary name (only this save writes there): network
        // shares (SMB) set a file's time when it is closed, so a stamp taken before would never
        // match the file read by name afterwards.
        drop(f);
        let m = std::fs::symlink_metadata(&tmp)?;
        let stamp = DiskStamp { modified: m.modified().ok(), len: m.len() };
        rename_over(&tmp, path)?;
        sync_dir(&dir);
        Ok(stamp)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Creates a new file readable and writable by its owner only.
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    opts.open(path)
}

/// Creates `dir` (and its parents) if needed and makes it private to its owner (0700 on Unix).
pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)?;
    #[cfg(unix)]
    std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    Ok(())
}

/// Renames `from` over `to`. On Windows a virus scanner or indexer can hold the destination open
/// for a moment, so the rename is retried briefly.
fn rename_over(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut attempt = 0;
    loop {
        match std::fs::rename(from, to) {
            Err(e) if cfg!(windows) && e.kind() == std::io::ErrorKind::PermissionDenied && attempt < 10 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            other => return other,
        }
    }
}

fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Lexically normalizes a path (resolves `.` and `..` without touching the filesystem).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Path of `target` relative to directory `base`, when both are absolute on the same root.
pub fn relative_path(target: &Path, base: &Path) -> PathBuf {
    let t = normalize(target);
    let b = normalize(base);
    if !t.is_absolute() || !b.is_absolute() {
        return t;
    }
    let tc: Vec<_> = t.components().collect();
    let bc: Vec<_> = b.components().collect();
    let common = tc.iter().zip(bc.iter()).take_while(|(a, b)| a == b).count();
    if common == 0 {
        return t;
    }
    let mut out = PathBuf::new();
    for _ in common..bc.len() {
        out.push("..");
    }
    for c in tc.iter().skip(common) {
        out.push(c.as_os_str());
    }
    out
}

/// Resolves a stored external path against the directory of the referencing document.
pub fn resolve_external_path(stored: &str, doc_path: Option<&Path>) -> PathBuf {
    let p = PathBuf::from(stored);
    if p.is_absolute() {
        return p;
    }
    match doc_path.and_then(|d| d.parent()) {
        Some(dir) => normalize(&dir.join(p)),
        None => p,
    }
}

fn snapshot(t: &Task) -> ExternalTaskSnapshot {
    ExternalTaskSnapshot {
        id: t.id,
        name: t.name.clone(),
        start: t.sched.start,
        finish: t.sched.finish,
        percent_complete: t.sched.percent_complete.round().clamp(0.0, 100.0) as u8,
        milestone: t.is_milestone(),
        duration: t.sched.duration,
    }
}

/// External task ids referenced by predecessors or milestone members.
pub fn referenced_external_tasks(p: &Project, project: Uuid) -> HashSet<TaskId> {
    let mut out = HashSet::new();
    for t in &p.tasks {
        for r in t.predecessors.iter().map(|d| &d.predecessor).chain(t.members.iter()) {
            if let TaskRef::External { project: pid, task } = r
                && *pid == project
            {
                out.insert(*task);
            }
        }
    }
    out
}

/// Registers another document as an external project and returns its id.
///
/// The stored path is relative to `doc_path`'s directory when `doc_path` is known.
pub fn add_external_project(p: &mut Project, doc_path: Option<&Path>, other: &Path) -> FileResult<Uuid> {
    let abs_other = absolute_path(other);
    let other_project = load(&abs_other)?;
    Ok(add_loaded_external_project(p, doc_path, &abs_other, &other_project))
}

/// `path` with every link and `..` resolved, as [`std::fs::canonicalize`], but in the form people
/// type: on Windows `C:\x` and `\\server\share\x`, not `\\?\C:\x` (kept when the
/// path needs it: longer than the old 260-character limit).
pub fn canonical(path: &Path) -> std::io::Result<PathBuf> {
    let full = std::fs::canonicalize(path)?;
    if cfg!(windows)
        && let Some(plain) = full.to_str().and_then(plain_windows_path)
    {
        return Ok(PathBuf::from(plain));
    }
    Ok(full)
}

/// A Windows `\\?\` path in its plain form, when it has one.
fn plain_windows_path(s: &str) -> Option<String> {
    let plain = if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        let rest = s.strip_prefix(r"\\?\")?;
        let mut chars = rest.chars();
        let drive = chars.next().is_some_and(|c| c.is_ascii_alphabetic()) && chars.next() == Some(':') && chars.next() == Some('\\');
        if !drive {
            return None;
        }
        rest.to_string()
    };
    (plain.chars().count() < 260).then_some(plain)
}

/// `path` made absolute against the current directory.
pub fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map(|c| c.join(path)).unwrap_or_else(|_| path.to_path_buf())
    }
}

/// [`add_external_project`] with the other document already read from `abs_other` (an absolute
/// path), so the file can be read without holding the workspace lock.
pub fn add_loaded_external_project(p: &mut Project, doc_path: Option<&Path>, abs_other: &Path, other_project: &Project) -> Uuid {
    let abs_other = abs_other.to_path_buf();
    let stored = match doc_path.and_then(|d| d.parent()) {
        Some(dir) => {
            let dir = if dir.is_absolute() { dir.to_path_buf() } else { std::env::current_dir().map(|c| c.join(dir)).unwrap_or_default() };
            relative_path(&abs_other, &dir)
        }
        None => abs_other.clone(),
    };
    let stored = stored.to_string_lossy().replace('\\', "/");
    if let Some(e) = p.external_projects.iter_mut().find(|e| e.id == other_project.id) {
        e.path = stored;
        e.name = other_project.name.clone();
        return e.id;
    }
    p.external_projects.push(ExternalProject {
        id: other_project.id,
        name: other_project.name.clone(),
        path: stored,
        tasks: Vec::new(),
        last_sync: Some(crate::dates::now()),
    });
    other_project.id
}

/// Ensures snapshot data exists for an external task (loading the other document).
pub fn ensure_external_task(p: &mut Project, doc_path: Option<&Path>, project: Uuid, task: TaskId) -> FileResult<()> {
    let Some(path) = external_path(p, doc_path, project) else {
        return Ok(());
    };
    let other = load_external(&path, project)?;
    ensure_external_task_from(p, project, &other, task);
    Ok(())
}

/// Where the linked project `project` is stored, resolved against `doc_path`.
pub fn external_path(p: &Project, doc_path: Option<&Path>, project: Uuid) -> Option<PathBuf> {
    p.external(project).map(|e| resolve_external_path(&e.path, doc_path))
}

/// Reads the document of the linked project `project` from `path`, refusing a file that now
/// holds a different project (it was replaced since the link was made).
pub fn load_external(path: &Path, project: Uuid) -> FileResult<Project> {
    let other = load(path)?;
    if other.id != project {
        return Err(FileError::DifferentProject { path: path.to_path_buf(), name: other.name });
    }
    Ok(other)
}

/// [`ensure_external_task`] with the linked document already read (see [`load_external`]).
pub fn ensure_external_task_from(p: &mut Project, project: Uuid, other: &Project, task: TaskId) {
    if let Some(ext) = p.external_projects.iter_mut().find(|e| e.id == project)
        && let Some(t) = other.task(task)
    {
        ext.tasks.retain(|s| s.id != task);
        ext.tasks.push(snapshot(t));
    }
}

/// Lists the tasks of an external project document (loading it from disk).
pub fn external_task_list(p: &Project, doc_path: Option<&Path>, project: Uuid) -> FileResult<Vec<ExternalTaskSnapshot>> {
    let Some(path) = external_path(p, doc_path, project) else {
        return Ok(Vec::new());
    };
    Ok(external_tasks_of(&load_external(&path, project)?))
}

/// The tasks of a linked document, as snapshots.
pub fn external_tasks_of(other: &Project) -> Vec<ExternalTaskSnapshot> {
    other.tasks.iter().map(snapshot).collect()
}

/// Whether a stored link path names a network share or device namespace (`\\server\share`,
/// `//server/share`, `\\?\…`). Following one makes Windows connect to that server and offer the
/// user's credentials, so a document from someone else must not trigger it just by being opened.
pub fn is_network_path(stored: &str) -> bool {
    let s = stored.trim_start();
    s.starts_with("\\\\") || s.starts_with("//") || s.starts_with("\\/") || s.starts_with("/\\")
}

/// Refreshes the cached dates of every referenced external task (explicit Refresh Links: all
/// links are followed). Returns warnings.
pub fn refresh_external(p: &mut Project, doc_path: Option<&Path>) -> Vec<String> {
    refresh_external_links(p, doc_path, true)
}

/// Refresh done automatically when a document opens: links to network shares are left for an
/// explicit Refresh Links (see [`is_network_path`]).
pub fn refresh_external_on_open(p: &mut Project, doc_path: Option<&Path>) -> Vec<String> {
    refresh_external_links(p, doc_path, false)
}

fn refresh_external_links(p: &mut Project, doc_path: Option<&Path>, follow_network: bool) -> Vec<String> {
    let reads = read_external_projects(&p.external_projects, doc_path, follow_network);
    apply_external_projects(p, reads)
}

/// The linked documents read for a refresh, made by [`read_external_projects`] and applied with
/// [`apply_external_projects`].
#[derive(Debug, Default)]
pub struct ExternalReads {
    /// For each followed link: its id, the resolved path, and the document read from it.
    docs: Vec<(Uuid, PathBuf, FileResult<Project>)>,
    /// Links that were not followed, as warnings.
    skipped: Vec<String>,
}

/// Reads the documents behind a plan's external links (an explicit refresh follows network-share
/// links too). Only the file reading happens here, so it can run without holding the workspace
/// lock; [`apply_external_projects`] then updates the snapshots.
pub fn read_external_projects(links: &[ExternalProject], doc_path: Option<&Path>, follow_network: bool) -> ExternalReads {
    let mut reads = ExternalReads::default();
    for e in links {
        if !follow_network && is_network_path(&e.path) {
            reads.skipped.push(format!("'{}' is on a network share ({}); use Refresh Links to update it", e.name, e.path));
            continue;
        }
        let path = resolve_external_path(&e.path, doc_path);
        let loaded = load(&path);
        reads.docs.push((e.id, path, loaded));
    }
    reads
}

/// Updates the cached snapshots of `p`'s external links from documents read with
/// [`read_external_projects`]. Returns warnings.
pub fn apply_external_projects(p: &mut Project, reads: ExternalReads) -> Vec<String> {
    let mut warnings = reads.skipped;
    let now = crate::dates::now();
    for (id, path, loaded) in reads.docs {
        let wanted = referenced_external_tasks(p, id);
        let Some(ext) = p.external_projects.iter_mut().find(|e| e.id == id) else { continue };
        match loaded {
            Ok(other) => {
                if other.id != id {
                    warnings.push(format!(
                        "{} now contains a different project ('{}'); links were not refreshed",
                        path.display(),
                        other.name
                    ));
                    continue;
                }
                ext.name = other.name.clone();
                ext.tasks.retain(|s| wanted.contains(&s.id));
                for tid in &wanted {
                    match other.task(*tid) {
                        Some(t) => {
                            ext.tasks.retain(|s| s.id != *tid);
                            ext.tasks.push(snapshot(t));
                        }
                        None => warnings.push(format!("Task {} no longer exists in '{}'", tid.0, other.name)),
                    }
                }
                ext.tasks.sort_by_key(|s| s.id);
                ext.last_sync = Some(now);
            }
            Err(e) => warnings.push(format!("Could not refresh '{}': {e}", ext.name)),
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    #[test]
    fn windows_paths_are_given_in_their_plain_form() {
        assert_eq!(plain_windows_path(r"\\?\C:\Users\Ana\Plans").as_deref(), Some(r"C:\Users\Ana\Plans"));
        assert_eq!(plain_windows_path(r"\\?\UNC\server\share\Plans").as_deref(), Some(r"\\server\share\Plans"));
        assert_eq!(plain_windows_path(r"C:\Plans"), None);
        assert_eq!(plain_windows_path(r"\\?\Volume{0b1c}\Plans"), None);
        let long = format!(r"\\?\C:\{}", "x".repeat(300));
        assert_eq!(plain_windows_path(&long), None, "too long for the plain form");
        let dir = std::env::temp_dir();
        assert!(canonical(&dir).unwrap().is_absolute());
    }

    use super::*;

    /// Regression: linking a task read whatever file now sits at the link's path, even when it
    /// holds a different project, and cached its task under the old project's id.
    #[test]
    fn a_link_to_a_replaced_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let start = crate::dates::ymd(2026, 10, 19).unwrap();
        let other_path = dir.path().join("other.plancraft");
        let mut other = Project::new("Other", start);
        let t = other.add_task("API", None);
        save(&other, &other_path).unwrap();
        let main_path = dir.path().join("main.plancraft");
        let mut p = Project::new("Main", start);
        let ext = add_external_project(&mut p, Some(&main_path), &other_path).unwrap();
        ensure_external_task(&mut p, Some(&main_path), ext, t).unwrap();

        let mut stranger = Project::new("Stranger", start);
        stranger.add_task("API", None);
        save(&stranger, &other_path).unwrap();
        p.external_projects.iter_mut().for_each(|e| e.tasks.clear());
        let e = ensure_external_task(&mut p, Some(&main_path), ext, t).unwrap_err();
        assert!(matches!(e, FileError::DifferentProject { .. }), "{e}");
        assert!(p.external(ext).unwrap().tasks.is_empty(), "nothing was cached from the other project");
        assert!(external_task_list(&p, Some(&main_path), ext).is_err());
    }

    /// Regression: opening a document read every path in its external links. A FIFO or device
    /// path blocked forever, and on Windows a `\\\\server\\share` path made the machine connect to
    /// that server (offering the user's credentials) just because a file was opened.
    #[test]
    fn hostile_link_paths_are_not_followed_on_open() {
        for net in ["\\\\attacker\\share\\x.plancraft", "//attacker/share/x.plancraft", " \\\\?\\UNC\\host\\x", "\\/host/x", "/\\host\\x"] {
            assert!(is_network_path(net), "{net}");
        }
        for local in ["other.plancraft", "../plans/b.plancraft", "/home/u/b.plancraft", "C:\\plans\\b.plancraft"] {
            assert!(!is_network_path(local), "{local}");
        }

        let dir = tempfile::tempdir().unwrap();
        let mut p = Project::new("A", crate::dates::ymd(2026, 10, 19).unwrap());
        p.external_projects.push(ExternalProject {
            id: Uuid::new_v4(),
            name: "Remote".into(),
            path: "\\\\attacker\\share\\x.plancraft".into(),
            tasks: Vec::new(),
            last_sync: None,
        });
        let doc = dir.path().join("a.plancraft");
        let w = refresh_external_on_open(&mut p, Some(&doc));
        assert!(w.len() == 1 && w[0].contains("network share"), "{w:?}");

        // A directory, and on Unix a FIFO, is refused at once instead of blocking.
        assert!(matches!(load(dir.path()), Err(FileError::Io { .. })));
        #[cfg(unix)]
        {
            let fifo = dir.path().join("pipe.plancraft");
            if std::process::Command::new("mkfifo").arg(&fifo).status().is_ok_and(|s| s.success()) {
                let started = std::time::Instant::now();
                let e = load(&fifo).unwrap_err().to_string();
                assert!(e.contains("not a regular file"), "{e}");
                assert!(started.elapsed() < std::time::Duration::from_secs(5));
            }
        }
    }

    #[test]
    fn atomic_save_replaces_and_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.plancraft");
        let mut p = Project::new("A", crate::dates::ymd(2026, 10, 19).unwrap());
        save(&p, &path).unwrap();
        p.name = "B".into();
        save(&p, &path).unwrap();
        assert_eq!(load(&path).unwrap().name, "B");
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from("plan.plancraft")]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600, "new plans are private");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
            save(&p, &path).unwrap();
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o640, "a replaced file keeps its mode");
            let private = dir.path().join("private/state");
            create_private_dir(&private).unwrap();
            assert_eq!(std::fs::metadata(&private).unwrap().permissions().mode() & 0o777, 0o700);
        }
    }

    /// A FIFO in place of a file is refused at once, not waited on forever.
    #[cfg(unix)]
    #[test]
    fn a_fifo_is_refused_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("plan.plancraft");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        if !made.is_ok_and(|s| s.success()) {
            return; // no mkfifo here
        }
        assert!(open_regular(&fifo, false, Links::Follow).is_err());
        assert!(open_regular(&fifo, true, Links::Follow).is_err());
        assert!(load(&fifo).is_err());
        let link = dir.path().join("link");
        std::fs::write(dir.path().join("real"), "x").unwrap();
        std::os::unix::fs::symlink(dir.path().join("real"), &link).unwrap();
        assert!(open_regular(&link, false, Links::Refuse).is_err());
        assert!(open_regular(&link, false, Links::Follow).is_ok());
    }

    /// A shared plan is never written through a link someone planted in its place, and keeps
    /// its permissions otherwise.
    #[cfg(unix)]
    #[test]
    fn no_links_refuses_a_link_and_keeps_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let plan = dir.path().join("plan.plancraft");
        std::fs::write(&plan, "old").unwrap();
        std::fs::set_permissions(&plan, std::fs::Permissions::from_mode(0o664)).unwrap();
        write_atomic(&plan, b"new", Privacy::NoLinks).unwrap();
        assert_eq!(std::fs::read_to_string(&plan).unwrap(), "new");
        assert_eq!(std::fs::metadata(&plan).unwrap().permissions().mode() & 0o777, 0o664);
        let target = dir.path().join("precious");
        std::fs::write(&target, "keep").unwrap();
        std::fs::remove_file(&plan).unwrap();
        std::os::unix::fs::symlink(&target, &plan).unwrap();
        assert!(write_atomic(&plan, b"evil", Privacy::NoLinks).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
        let fresh = dir.path().join("fresh.plancraft");
        write_atomic(&fresh, b"x", Privacy::NoLinks).unwrap();
        assert_eq!(std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777, 0o600);
    }

    /// Regression: saving through a symbolic link replaced the link with a regular file, leaving
    /// the real plan it pointed at unchanged.
    #[cfg(unix)]
    #[test]
    fn saving_through_a_symlink_updates_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let real_dir = dir.path().join("real");
        std::fs::create_dir(&real_dir).unwrap();
        let target = real_dir.join("plan.plancraft");
        let mut p = Project::new("A", crate::dates::ymd(2026, 10, 19).unwrap());
        save(&p, &target).unwrap();
        // A relative link, and a link to that link.
        let link = dir.path().join("link.plancraft");
        std::os::unix::fs::symlink("real/plan.plancraft", &link).unwrap();
        let link2 = dir.path().join("link2.plancraft");
        std::os::unix::fs::symlink(&link, &link2).unwrap();

        p.name = "B".into();
        save(&p, &link2).unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "the link stays a link");
        assert!(std::fs::symlink_metadata(&link2).unwrap().file_type().is_symlink(), "the link stays a link");
        assert_eq!(load(&target).unwrap().name, "B", "the target was updated");
        let names: Vec<_> = std::fs::read_dir(&real_dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from("plan.plancraft")], "no temporary file is left behind");

        // A loop of links is refused, not followed forever.
        let a = dir.path().join("a.plancraft");
        let b = dir.path().join("b.plancraft");
        std::os::unix::fs::symlink(&b, &a).unwrap();
        std::os::unix::fs::symlink(&a, &b).unwrap();
        assert!(save(&p, &a).is_err());
    }

    /// Regression: a read-only plan (mode 0444) was silently replaced on save.
    #[test]
    fn a_read_only_plan_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.plancraft");
        let mut p = Project::new("A", crate::dates::ymd(2026, 10, 19).unwrap());
        save(&p, &path).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&path, perms).unwrap();

        p.name = "B".into();
        let e = save(&p, &path).unwrap_err();
        assert!(e.to_string().contains("read-only"), "{e}");
        assert!(matches!(&e, FileError::Io { source, .. } if source.kind() == std::io::ErrorKind::PermissionDenied), "{e}");
        assert_eq!(load(&path).unwrap().name, "A", "the file was left alone");
        let count = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(count, 1, "no temporary file is left behind");
        // Private files (settings, recovery copies) are still replaced (Windows refuses to rename
        // over a read-only file whatever we do).
        #[cfg(unix)]
        {
            write_atomic(&path, b"{}", Privacy::OwnerOnly).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), b"{}");
        }
    }

    /// Regression: a replaced file lost its group (it got the user's primary group). Needs a
    /// second group the user belongs to; without one there is nothing to check.
    #[cfg(unix)]
    #[test]
    fn a_replaced_plan_keeps_its_group() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.plancraft");
        let p = Project::new("A", crate::dates::ymd(2026, 10, 19).unwrap());
        save(&p, &path).unwrap();
        let primary = std::fs::metadata(&path).unwrap().gid();
        let groups = std::process::Command::new("id").arg("-G").output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
        let other = groups.unwrap_or_default().split_whitespace().filter_map(|g| g.parse::<u32>().ok()).find(|&g| g != primary);
        let Some(other) = other.filter(|&g| std::os::unix::fs::chown(&path, None, Some(g)).is_ok()) else {
            eprintln!("skipped: the user belongs to no second group");
            return;
        };
        save(&p, &path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().gid(), other, "the group was kept");
    }

    #[test]
    fn hostile_files_are_refused_or_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.plancraft");
        let mut p = Project::new("A", crate::dates::ymd(2026, 10, 19).unwrap());
        let a = p.add_task("A", None);
        let mut json: serde_json::Value = serde_json::from_str(&to_json(&p).unwrap()).unwrap();
        json["tasks"][0]["duration"] = serde_json::json!(i64::MAX);
        json["tasks"][0]["percent_complete"] = serde_json::json!(255);
        json["start"] = serde_json::json!("+200000-01-01T08:00:00");
        std::fs::write(&path, json.to_string()).unwrap();
        let (q, notes) = load_with_notes(&path).unwrap();
        assert_eq!(q.task(a).unwrap().duration, limits::MAX_MINUTES);
        assert_eq!(q.task(a).unwrap().percent_complete, 100);
        assert_eq!(q.start.date(), limits::max_date());
        assert!(!notes.is_empty());
        // Years `jiff` can't read as written open too, clamped into range (as with `chrono`), and
        // only whole dates are touched: a name that starts like one keeps its text.
        for (start, day) in [
            ("-200000-01-01T08:00:00", limits::min_date()),
            ("-10000-01-01T08:00:00", limits::min_date()),
            ("-0001-01-01T08:00:00", limits::min_date()),
            ("+2026-10-19T08:00:00", crate::dates::ymd(2026, 10, 19).unwrap()),
            ("+10000-01-01", limits::max_date()),
        ] {
            json["start"] = serde_json::json!(start);
            json["name"] = serde_json::json!("+123456-01-01 launch");
            std::fs::write(&path, json.to_string()).unwrap();
            let (q, _) = load_with_notes(&path).unwrap_or_else(|e| panic!("{start}: {e}"));
            assert_eq!(q.start.date(), day, "{start}");
            assert_eq!(q.name, "+123456-01-01 launch", "{start}");
        }

        json["version"] = serde_json::json!(u64::from(u32::MAX) + 1);
        std::fs::write(&path, json.to_string()).unwrap();
        assert!(matches!(load(&path), Err(FileError::TooNew { .. })));

        json["version"] = serde_json::json!(1);
        let task = json["tasks"][0].clone();
        json["tasks"] = serde_json::Value::Array(vec![task; 2]);
        std::fs::write(&path, json.to_string()).unwrap();
        assert!(matches!(load(&path), Err(FileError::Limit { .. })), "duplicate ids are refused");

        let deep = format!("{{\"format\":\"plancraft\",\"x\":{}{}}}", "[".repeat(100_000), "]".repeat(100_000));
        std::fs::write(&path, deep).unwrap();
        assert!(load(&path).is_err());
    }

    #[test]
    fn id_counters_behind_the_ids_are_raised_on_open() {
        // A copy of a shared plan (or a hand-edited file) may count from below ids others used.
        let mut p = crate::sample::demo_project(crate::dates::ymd(2026, 1, 5).unwrap());
        let field = p.add_custom_field("Phase", crate::custom::CustomFieldKind::Text, &[]).unwrap();
        p.ids.task = 0;
        p.ids.custom_field = 0;
        let (back, _) = from_json_with_notes(&to_json(&p).unwrap(), Path::new("x.plancraft")).unwrap();
        assert!(back.ids.task >= back.tasks.iter().map(|t| t.id.0).max().unwrap());
        assert!(back.ids.custom_field >= field.0, "custom field counter {} below field {}", back.ids.custom_field, field.0);
    }

    /// Regression: a save read its file's stamp by name after the rename, so another program's
    /// write landing in between passed for the save (never reported, never refused).
    #[test]
    fn a_save_knows_its_own_file_by_the_file_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.plancraft");
        let p = crate::sample::demo_project(crate::dates::ymd(2026, 1, 5).unwrap());
        let stamp = save_stamped(&p, &path).unwrap();
        assert_eq!(Some(stamp), disk_stamp(&path), "the rename keeps the time and size");
        std::fs::write(&path, "written by someone else, longer than before or not").unwrap();
        assert_ne!(Some(stamp), disk_stamp(&path));
    }

    #[test]
    fn relative_paths() {
        // An absolute root on this platform ("/a" is not absolute on Windows).
        let root = PathBuf::from(if cfg!(windows) { "C:\\" } else { "/" }).join("a");
        let up = Path::new("..");
        assert_eq!(relative_path(&root.join("b").join("c.plancraft"), &root.join("b")), PathBuf::from("c.plancraft"));
        assert_eq!(relative_path(&root.join("x").join("c.plancraft"), &root.join("b")), up.join("x").join("c.plancraft"));
        assert_eq!(
            resolve_external_path("../x/c.plancraft", Some(&root.join("b").join("doc.plancraft"))),
            root.join("x").join("c.plancraft")
        );
    }
}
