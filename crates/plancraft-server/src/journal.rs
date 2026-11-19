//! The journal next to a shared plan (`.NAME.journal`): what happened since the plan was last
//! saved, written to disk before anyone hears of it, so a crash or power cut loses nothing.
//!
//! Each line is one JSON record. The first is a [`Record::Header`]: the version the plan file was
//! saved at, a hash of that file, and the ids of the changes accepted before (see
//! `Host::resume`). Then, in order:
//!
//! - every accepted change ([`Record::Entry`]);
//! - every block of ids handed out, written *before* it is handed out ([`Record::Lease`]);
//! - [`Record::Saving`] just before the plan file is replaced, with the new file's version and
//!   hash. If the power fails after the plan is written but before the journal is started
//!   afresh, the hash tells which version the file on disk is.
//!
//! A line cut short by a crash ends the journal; a damaged line in the middle stops the replay
//! there, and the journal is kept aside for a person to look at.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use blake2::{Blake2s256, Digest};
use plancraft_core::ids::IdCounters;
use plancraft_core::sync::{ChangeId, Entry, Version};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::protocol::MAX_CLIENT_FRAME;
use crate::wire::to_hex;

/// Largest journal read (between two saves it holds a few minutes of changes).
pub const MAX_JOURNAL_BYTES: u64 = 1024 * 1024 * 1024;
/// Longest line read (one change, which arrived in one frame, plus its framing).
const MAX_LINE_BYTES: u64 = 2 * MAX_CLIENT_FRAME as u64;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum Record {
    Header {
        epoch: Uuid,
        version: Version,
        /// [`plan_hash`] of the plan file as saved at `version`.
        plan_hash: String,
        #[serde(default)]
        applied: Vec<ChangeId>,
    },
    Entry {
        entry: Entry,
    },
    Lease {
        start: IdCounters,
    },
    Saving {
        version: Version,
        plan_hash: String,
    },
}

/// The hash that identifies a plan file's contents.
pub fn plan_hash(bytes: &[u8]) -> String {
    to_hex(&Blake2s256::digest(bytes))
}

/// The journal of the plan at `plan`.
pub fn path_for(plan: &Path) -> Option<PathBuf> {
    let mut name = std::ffi::OsString::from(".");
    name.push(plan.file_name()?);
    name.push(".journal");
    Some(plan.with_file_name(name))
}

/// A journal as read back.
#[derive(Debug, Default)]
pub struct ReadBack {
    pub records: Vec<Record>,
    /// A line in the middle could not be read: the records after it were left out.
    pub damaged: bool,
}

/// Reads the journal at `path` (none: no records).
pub fn read(path: &Path) -> std::io::Result<ReadBack> {
    // No check of the name first: `open_regular` checks the file it opened (a link, FIFO or
    // folder put there is refused, and the open never blocks).
    let file = match plancraft_core::io::open_regular(path, false, plancraft_core::io::Links::Refuse) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ReadBack::default()),
        Err(e) => return Err(e),
    };
    if file.metadata()?.len() > MAX_JOURNAL_BYTES {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "the journal is too large"));
    }
    let mut reader = BufReader::new(file);
    let mut out = ReadBack::default();
    loop {
        let mut line = Vec::new();
        let n = (&mut reader).take(MAX_LINE_BYTES).read_until(b'\n', &mut line)?;
        if n == 0 {
            break;
        }
        let complete = line.last() == Some(&b'\n');
        match serde_json::from_slice::<Record>(&line) {
            Ok(r) if complete => out.records.push(r),
            // The last line, cut short by a crash: it was never confirmed to anyone.
            _ if !complete => break,
            _ => {
                out.damaged = true;
                break;
            }
        }
    }
    Ok(out)
}

fn line(record: &Record) -> std::io::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(record).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// An open journal, appended to.
pub struct Journal {
    path: PathBuf,
    file: File,
    /// Bytes written to it since it started.
    written: u64,
}

/// Writes `bytes` to a new private file next to `path` and renames it over `path`, keeping the
/// file open: the journal is written through the handle that made it, never by opening the path
/// again (where a link planted in between would be followed).
fn replace_keeping_open(path: &Path, bytes: &[u8]) -> std::io::Result<File> {
    use std::io::Write;
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let name = path.file_name().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a file path"))?;
    for n in 0..100u32 {
        let tmp = dir.join(format!(".{}.{}-{n}.tmp", name.to_string_lossy(), std::process::id()));
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut file = match opts.open(&tmp) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        let done = file.write_all(bytes).and_then(|()| file.sync_all()).and_then(|()| std::fs::rename(&tmp, path));
        if let Err(e) = done {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        #[cfg(unix)]
        if let Ok(d) = File::open(&dir) {
            let _ = d.sync_all();
        }
        return Ok(file);
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "no free name for the journal's temporary file"))
}

impl Journal {
    /// Starts the journal afresh with `header` (replacing it in one step).
    pub fn start(path: &Path, header: &Record) -> std::io::Result<Journal> {
        Self::start_with(path, std::slice::from_ref(header))
    }

    /// Starts the journal afresh with `records` (replacing it in one step).
    pub fn start_with(path: &Path, records: &[Record]) -> std::io::Result<Journal> {
        let mut bytes = Vec::new();
        for r in records {
            bytes.extend(line(r)?);
        }
        let file = replace_keeping_open(path, &bytes)?;
        Ok(Journal { path: path.to_path_buf(), file, written: bytes.len() as u64 })
    }

    /// Adds a record and waits until it is on disk.
    pub fn append(&mut self, record: &Record) -> std::io::Result<()> {
        let bytes = line(record)?;
        self.written = self.written.saturating_add(bytes.len() as u64);
        self.file.write_all(&bytes)?;
        self.file.sync_data()
    }

    /// Bytes written since the journal started.
    pub fn written(&self) -> u64 {
        self.written
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A journal that writes to `file` instead (tests: a full disk).
    #[cfg(test)]
    pub fn on(path: &Path, file: File) -> Journal {
        Journal { path: path.to_path_buf(), file, written: 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(v: Version) -> Record {
        Record::Header { epoch: Uuid::nil(), version: v, plan_hash: "h".into(), applied: vec![ChangeId(Uuid::nil())] }
    }

    #[test]
    fn records_read_back_and_a_torn_tail_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".plan.plancraft.journal");
        let mut j = Journal::start(&path, &header(3)).unwrap();
        j.append(&Record::Lease { start: IdCounters::default() }).unwrap();
        j.append(&Record::Saving { version: 3, plan_hash: "x".into() }).unwrap();
        let back = read(&path).unwrap();
        assert_eq!(back.records.len(), 3);
        assert!(!back.damaged);
        // A crash in the middle of a write leaves half a line.
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{\"record\":\"saving\",\"vers").unwrap();
        let back = read(&path).unwrap();
        assert_eq!(back.records.len(), 3);
        assert!(!back.damaged, "a torn last line is normal after a crash");
        // Starting afresh replaces everything, and later records go to the new file.
        let mut j = Journal::start(&path, &header(4)).unwrap();
        j.append(&Record::Saving { version: 4, plan_hash: "y".into() }).unwrap();
        assert_eq!(read(&path).unwrap().records, vec![header(4), Record::Saving { version: 4, plan_hash: "y".into() }]);
        assert!(j.written() > 0);
    }

    /// A link planted at the journal's name is replaced, never written through.
    #[cfg(unix)]
    #[test]
    fn a_link_in_place_of_the_journal_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".plan.plancraft.journal");
        let target = dir.path().join("precious");
        std::fs::write(&target, "keep").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        let mut j = Journal::start(&path, &header(1)).unwrap();
        j.append(&Record::Lease { start: IdCounters::default() }).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
        assert_eq!(read(&path).unwrap().records.len(), 2);
    }

    #[test]
    fn a_damaged_line_in_the_middle_stops_the_reading() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("j");
        let mut text = String::from_utf8(line(&header(1)).unwrap()).unwrap();
        text.push_str("garbage\n");
        text.push_str(&String::from_utf8(line(&header(2)).unwrap()).unwrap());
        std::fs::write(&path, text).unwrap();
        let back = read(&path).unwrap();
        assert_eq!(back.records, vec![header(1)]);
        assert!(back.damaged);
    }

    #[test]
    fn no_journal_is_an_empty_one_and_odd_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(&dir.path().join("none")).unwrap().records.is_empty());
        assert!(read(dir.path()).is_err(), "a folder is not a journal");
        assert_eq!(path_for(Path::new("/a/Plan.plancraft")), Some(PathBuf::from("/a/.Plan.plancraft.journal")));
    }
}
