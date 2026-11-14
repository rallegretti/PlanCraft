//! Microsoft Project XML (MSPDI) import and export.
//!
//! Follows Microsoft's published *Project XML Data Interchange Schema Reference*
//! (learn.microsoft.com/office-project/xml-data-interchange), available under Microsoft's Open
//! Specification Promise, and the `PjField` enumeration for custom field ids. Clean-room: no
//! Microsoft code, files or product output were used.
//!
//! What maps: project properties and time settings, calendars (work weeks and exceptions,
//! recurring ones expanded into dates), the task outline, durations, task types, constraints,
//! deadlines, progress, manual scheduling, inactive tasks, notes, links with lag, baselines 0–10
//! and interim plans, task custom fields (Text, Number, Cost, Date, Duration, Flag), resources
//! (work, material, cost; rates, availability, personal calendars) and assignments.
//! What does not is listed in the notes each direction returns.

pub mod codes;
mod read;
mod write;
pub mod xml;

use std::io::Read as _;
use std::path::Path;

use crate::io::{FileError, FileResult, Privacy, write_atomic};
use crate::limits::MAX_FILE_BYTES;
use crate::model::Project;

/// The file extension of Microsoft Project XML files.
pub const EXTENSION: &str = "xml";

/// Whether a path names a Microsoft Project XML file (by its extension).
pub fn is_mspdi_path(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION))
}

/// Whether an existing file is a Microsoft Project XML file: its first element is `<Project>`
/// (looking only at the start of the file).
pub fn is_mspdi_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if !meta.is_file() {
        return false;
    }
    let Ok(file) = std::fs::File::open(path) else { return false };
    let mut head = Vec::new();
    if file.take(64 * 1024).read_to_end(&mut head).is_err() {
        return false;
    }
    let Some(text) = decode(&head).or_else(|| Some(String::from_utf8_lossy(&head).into_owned())) else { return false };
    first_element(&text).is_some_and(|name| name == "Project")
}

/// The local name of the first element in XML text (after the declaration, comments and a
/// DOCTYPE).
fn first_element(text: &str) -> Option<&str> {
    let mut rest = text.trim_start_matches('\u{feff}');
    loop {
        rest = rest.trim_start();
        if let Some(r) = rest.strip_prefix("<?") {
            rest = r.split_once("?>")?.1;
        } else if let Some(r) = rest.strip_prefix("<!--") {
            rest = r.split_once("-->")?.1;
        } else if let Some(r) = rest.strip_prefix("<!") {
            rest = r.split_once('>')?.1;
        } else {
            let tag = rest.strip_prefix('<')?;
            let name = tag.split(|c: char| c.is_whitespace() || c == '>' || c == '/').next()?;
            return Some(name.rsplit(':').next().unwrap_or(name));
        }
    }
}

/// Reads a plan from MSPDI text, treating it as untrusted. `fallback_name` names the plan when
/// the file does not. Returns the plan (not yet recalculated) and notes on what could not be
/// carried over or was clamped into range.
pub fn from_xml(text: &str, fallback_name: &str) -> Result<(Project, Vec<String>), String> {
    crate::guard::catch("reading a Microsoft Project XML file", || read::import(text, fallback_name))
        .map_err(|p| format!("PlanCraft hit an internal error reading the file ({})", p.message))?
}

/// Writes a plan as MSPDI text. Returns the XML and notes on what MSPDI cannot carry.
pub fn to_xml(p: &Project) -> (String, Vec<String>) {
    write::export(p)
}

/// Decodes a file's bytes: UTF-8 (with or without a byte order mark) or UTF-16 with a byte
/// order mark.
fn decode(bytes: &[u8]) -> Option<String> {
    let utf16 = |rest: &[u8], le: bool| {
        let units = rest.as_chunks::<2>().0.iter().map(|c| if le { u16::from_le_bytes(*c) } else { u16::from_be_bytes(*c) });
        char::decode_utf16(units).collect::<Result<String, _>>().ok()
    };
    match bytes {
        [0xFF, 0xFE, rest @ ..] => utf16(rest, true),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, false),
        _ => String::from_utf8(bytes.to_vec()).ok(),
    }
}

/// Reads a plan from the bytes of an MSPDI file (UTF-8, or UTF-16 with a byte order mark),
/// treating them as untrusted. The plan is not yet recalculated.
pub fn from_bytes(bytes: &[u8], fallback_name: &str) -> Result<(Project, Vec<String>), String> {
    let text = decode(bytes).ok_or_else(|| "the file is not UTF-8 or UTF-16 text".to_string())?;
    from_xml(&text, fallback_name)
}

/// Imports a Microsoft Project XML file and recalculates it. Returns the plan and notes.
pub fn load(path: impl AsRef<Path>) -> FileResult<(Project, Vec<String>)> {
    let path = path.as_ref();
    let io_err = |source| FileError::Io { path: path.into(), source };
    if !std::fs::metadata(path).map_err(io_err)?.is_file() {
        return Err(io_err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file")));
    }
    let file = std::fs::File::open(path).map_err(io_err)?;
    let size = file.metadata().map_err(io_err)?.len();
    if size > MAX_FILE_BYTES {
        return Err(FileError::TooLarge { path: path.into(), size });
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes).map_err(io_err)?;
    let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if size > MAX_FILE_BYTES {
        return Err(FileError::TooLarge { path: path.into(), size });
    }
    let import = |message: String| FileError::Import { path: path.into(), message };
    let fallback = path.file_stem().map_or_else(|| "Imported plan".to_string(), |s| s.to_string_lossy().into_owned());
    let (mut p, notes) = from_bytes(&bytes, &fallback).map_err(import)?;
    crate::schedule::try_recalculate(&mut p).map_err(|e| import(format!("the plan could not be scheduled ({})", e.message)))?;
    Ok((p, notes))
}

/// Exports a plan to a Microsoft Project XML file (atomically). Returns notes on what was left out.
pub fn save(p: &Project, path: impl AsRef<Path>) -> FileResult<Vec<String>> {
    let path = path.as_ref();
    let (xml, notes) = to_xml(p);
    write_atomic(path, xml.as_bytes(), Privacy::KeepExisting).map_err(|source| FileError::Io { path: path.into(), source })?;
    Ok(notes)
}
