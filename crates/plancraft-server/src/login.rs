//! Keeping a folder shared from login on, for one person and with no administrator rights
//! (docs/COLLABORATION.md › Phase 5): PlanCraft's *Share Folder › Keep sharing* starts the
//! server in the background ([`crate::background`]) and has the system start it again whenever
//! its owner logs in:
//!
//! - Linux: a systemd user unit, `~/.config/systemd/user/plancraft-server.service`;
//! - macOS: a LaunchAgent, `~/Library/LaunchAgents/dev.plancraft.server.plist`;
//! - Windows: a value in the person's `Run` key (Task Scheduler's logon tasks need an
//!   administrator).
//!
//! Each is the person's own, and nothing else is installed. [`disable`] takes it away again.
//! Stopping goes through the server's stop request, the same everywhere, before the login item
//! is taken away (taking a LaunchAgent away signals the server); systemd and launchd start it
//! again only after a failure, never after a clean stop, and systemd gives up after a few.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::sharing::Program;

/// The systemd user unit's name.
pub const SYSTEMD_UNIT: &str = "plancraft-server.service";
/// The LaunchAgent's label.
pub const LAUNCHD_LABEL: &str = "dev.plancraft.server";
/// The name of the value in the `Run` key.
pub const RUN_VALUE: &str = "PlanCraft Server";
/// Where the `Run` key is, under the person's registry hive.
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// The bundle the LaunchAgent belongs to (System Settings › Login Items names it after it).
const MAC_BUNDLE_ID: &str = "dev.plancraft.PlanCraft";
/// Seconds a service manager waits before starting a server that failed again.
const RESTART_SECS: u32 = 30;
/// How long a service manager's command may take.
const TOOL_WAIT: Duration = Duration::from_secs(30);
/// Failed starts within [`START_LIMIT_SECS`] after which systemd stops trying (a server that
/// cannot start, or a program that was removed).
const START_LIMIT: u32 = 5;
const START_LIMIT_SECS: u32 = 600;
/// Seconds launchd gives a server it stops (logging out) to save its plans.
const EXIT_SECS: u32 = 60;
/// The longest command line Windows runs from the `Run` key.
const RUN_LINE_MAX: usize = 260;

/// The command that keeps a folder shared: the server program and everything it is given.
#[derive(Clone, Debug)]
pub struct Entry {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    /// The shared folder's name, for the service's description.
    pub name: String,
    /// The server's private folder.
    pub config: PathBuf,
    /// Adjusts the commands run here (the app gives back the environment it was started with).
    pub prepare: Option<fn(&mut Command)>,
}

impl Entry {
    /// The command sharing `folder` on `port` from `config`, run by `program`.
    pub fn new(program: &Program, folder: &Path, config: &Path, port: u16, announce: bool) -> Result<Entry, String> {
        let folder = plancraft_core::io::canonical(folder).map_err(|e| format!("cannot open the folder {}: {e}", folder.display()))?;
        if !program.program.is_absolute() || !config.is_absolute() {
            return Err("cannot tell where PlanCraft or its settings are".into());
        }
        if let Some(why) = temporary_place(&program.program) {
            return Err(why.into());
        }
        let mut args = program.args.clone();
        args.push("--background".into());
        // Windows runs at most 260 characters from the Run key: the usual folder goes unsaid.
        if !(cfg!(windows) && crate::server::default_config().as_deref() == Some(config)) {
            args.extend(["--config".into(), config.as_os_str().to_os_string()]);
        }
        args.extend(["--port".into(), port.to_string().into()]);
        if !announce {
            args.push("--no-announce".into());
        }
        // `--`: a folder whose name starts with "-" is still the folder.
        args.extend(["--".into(), folder.as_os_str().to_os_string()]);
        let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| folder.display().to_string());
        Ok(Entry { program: program.program.clone(), args, name, config: config.to_path_buf(), prepare: program.prepare })
    }

    /// The program and its arguments, as text.
    fn words(&self) -> Result<Vec<String>, String> {
        std::iter::once(self.program.as_os_str())
            .chain(self.args.iter().map(OsString::as_os_str))
            .map(|w| {
                let w = w.to_str().ok_or("a folder's name is not valid text, so it cannot be shared at login")?;
                if w.chars().any(char::is_control) {
                    return Err("a folder's name holds a line break or another control character, so it cannot be shared at login".into());
                }
                Ok(w.to_string())
            })
            .collect()
    }
}

/// Why `program` would be gone by the next login: it runs from a temporary place.
fn temporary_place(program: &Path) -> Option<&'static str> {
    let text = program.to_string_lossy();
    if cfg!(target_os = "macos") && (text.contains("/AppTranslocation/") || text.starts_with("/Volumes/")) {
        return Some(
            "PlanCraft is running from the disk image or a temporary place: move it into Applications, open it from there, and try again",
        );
    }
    if in_folder(program, &std::env::temp_dir()) {
        return Some(
            "PlanCraft is running from a temporary folder (unzipped by the system?): put it in a folder of its own, open it from there, and try again",
        );
    }
    None
}

/// Whether `path` is in `folder`, also by their real names (a Windows short name such as
/// `C:\Users\ROBERT~1`, macOS's `/private/var`), with Windows' names in any case.
fn in_folder(path: &Path, folder: &Path) -> bool {
    let real = |p: &Path| plancraft_core::io::canonical(p).unwrap_or_else(|_| p.to_path_buf());
    let inside = |p: &Path, f: &Path| {
        if cfg!(windows) {
            Path::new(&p.to_string_lossy().to_lowercase()).starts_with(Path::new(&f.to_string_lossy().to_lowercase()))
        } else {
            p.starts_with(f)
        }
    };
    inside(path, folder) || inside(&real(path), &real(folder))
}

/// One word of a systemd command line: quoted, with systemd's specifiers (`%`) and variables
/// (`$`) kept as they are.
fn systemd_word(w: &str) -> String {
    let mut out = String::with_capacity(w.len() + 2);
    out.push('"');
    for c in w.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '%' => out.push_str("%%"),
            '$' => out.push_str("$$"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The systemd user unit for `entry`.
pub fn systemd_unit(entry: &Entry) -> Result<String, String> {
    let words = entry.words()?;
    let exec: Vec<String> = words.iter().map(|w| systemd_word(w)).collect();
    let name: String = entry.name.chars().filter(|c| !c.is_control()).map(|c| if c == '%' { "%%".into() } else { c.to_string() }).collect();
    Ok(format!(
        "# Written by PlanCraft (File > Share Folder > Keep sharing); Stop Sharing there removes it.\n\
         [Unit]\n\
         Description=PlanCraft Server sharing {name}\n\
         StartLimitIntervalSec={START_LIMIT_SECS}\n\
         StartLimitBurst={START_LIMIT}\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={}\n\
         Restart=on-failure\n\
         RestartSec={RESTART_SECS}\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exec.join(" ")
    ))
}

fn xml_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// The LaunchAgent for `entry`.
pub fn launchd_plist(entry: &Entry) -> Result<String, String> {
    let words = entry.words()?;
    let args: String = words.iter().map(|w| format!("        <string>{}</string>\n", xml_text(w))).collect();
    let dir = entry.config.to_str().ok_or("the settings folder's name is not valid text")?;
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <!-- Written by PlanCraft (File > Share Folder > Keep sharing); Stop Sharing there removes it. -->\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \x20   <key>Label</key>\n\
         \x20   <string>{LAUNCHD_LABEL}</string>\n\
         \x20   <key>ProgramArguments</key>\n\
         \x20   <array>\n\
         {args}\
         \x20   </array>\n\
         \x20   <key>WorkingDirectory</key>\n\
         \x20   <string>{}</string>\n\
         \x20   <key>RunAtLoad</key>\n\
         \x20   <true/>\n\
         \x20   <key>KeepAlive</key>\n\
         \x20   <dict>\n\
         \x20       <key>SuccessfulExit</key>\n\
         \x20       <false/>\n\
         \x20   </dict>\n\
         \x20   <key>ThrottleInterval</key>\n\
         \x20   <integer>{RESTART_SECS}</integer>\n\
         \x20   <key>ExitTimeOut</key>\n\
         \x20   <integer>{EXIT_SECS}</integer>\n\
         \x20   <key>AssociatedBundleIdentifiers</key>\n\
         \x20   <string>{MAC_BUNDLE_ID}</string>\n\
         </dict>\n\
         </plist>\n",
        xml_text(dir)
    ))
}

/// One argument of a Windows command line, quoted so the program reads it back as it was.
fn windows_arg(a: &str) -> String {
    let mut out = String::with_capacity(a.len() + 2);
    out.push('"');
    let mut slashes = 0usize;
    for c in a.chars() {
        match c {
            '\\' => slashes = slashes.saturating_add(1),
            '"' => {
                // Backslashes before a quote are doubled, and the quote escaped.
                out.extend(std::iter::repeat_n('\\', slashes.saturating_mul(2).saturating_add(1)));
                out.push('"');
                slashes = 0;
            }
            c => {
                out.extend(std::iter::repeat_n('\\', slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    // Before the closing quote, backslashes are doubled.
    out.extend(std::iter::repeat_n('\\', slashes.saturating_mul(2)));
    out.push('"');
    out
}

/// The command line put in the `Run` key for `entry`.
pub fn windows_command_line(entry: &Entry) -> Result<String, String> {
    let words = entry.words()?;
    if words.iter().any(|w| w.contains('%')) {
        // The system may read %NAME% in it as a variable.
        return Err("a folder whose name holds % cannot be shared at login on Windows".into());
    }
    let mut it = words.iter();
    // The program is read up to the next quote (file names hold none).
    let mut line = it.next().map(|p| format!("\"{p}\"")).unwrap_or_default();
    for w in it {
        line.push(' ');
        // Plain words (`serve`, `--port`, its number) go as they are: the line is kept short.
        if !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
            line.push_str(w);
        } else {
            line.push_str(&windows_arg(w));
        }
    }
    let length = line.encode_utf16().count();
    if length > RUN_LINE_MAX {
        return Err(format!(
            "the folder's path is too long to share at login on Windows (the command would be {length} characters, and Windows \
             runs at most {RUN_LINE_MAX}): share a folder with a shorter path"
        ));
    }
    Ok(line)
}

/// Runs a service manager's command; what it said, or why it failed.
#[cfg_attr(windows, allow(dead_code))]
fn run_tool(mut cmd: Command, prepare: Option<fn(&mut Command)>) -> Result<String, String> {
    if let Some(p) = prepare {
        p(&mut cmd);
    }
    let shown =
        format!("{} {}", cmd.get_program().to_string_lossy(), cmd.get_args().map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" "));
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { format!("{shown}: not found") } else { format!("{shown}: {e}") })?;
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let deadline = Instant::now() + TOOL_WAIT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{shown}: no answer after {} seconds", TOOL_WAIT.as_secs()));
            }
            Err(e) => return Err(format!("{shown}: {e}")),
        }
    };
    let out = out.join().unwrap_or_default();
    let err = err.join().unwrap_or_default();
    if status.success() {
        Ok(out)
    } else {
        let said = err.trim();
        let said = if said.is_empty() { out.trim() } else { said };
        Err(format!("{shown}: {}", if said.is_empty() { status.to_string() } else { said.lines().next().unwrap_or_default().to_string() }))
    }
}

/// Reads what a command says, on a thread of its own (at most 64 KiB of it).
#[cfg_attr(windows, allow(dead_code))]
fn drain<R: std::io::Read + Send + 'static>(r: Option<R>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(r) = r {
            let _ = std::io::Read::read_to_string(&mut r.take(64 * 1024), &mut s);
        }
        s
    })
}

/// Writes a file that is the person's own (owner-only), making its folder if needed.
#[cfg_attr(windows, allow(dead_code))]
fn write_own(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    plancraft_core::io::write_atomic(path, text.as_bytes(), plancraft_core::io::Privacy::OwnerOnly)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Where the systemd user unit goes.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn systemd_unit_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")).filter(|p| p.is_absolute()))?;
    Some(base.join("systemd").join("user").join(SYSTEMD_UNIT))
}

/// Where the LaunchAgent goes.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn launch_agent_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_absolute())?;
    Some(home.join("Library").join("LaunchAgents").join(format!("{LAUNCHD_LABEL}.plist")))
}

/// Has the system start the server of `entry` at every login, and starts it now. One the
/// system started before is stopped first (saving its plans); stop any other one first
/// ([`crate::background::request_stop`]), or the new one finds it sharing and leaves.
pub fn enable(entry: &Entry) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let path = systemd_unit_path().ok_or("cannot tell where your settings folder is (HOME is not set)")?;
        let unit = systemd_unit(entry)?;
        write_own(&path, &unit)?;
        let systemctl = |args: &[&str]| {
            let mut c = Command::new("systemctl");
            c.arg("--user").args(args);
            run_tool(c, entry.prepare)
        };
        let started = systemctl(&["daemon-reload"])
            .and_then(|_| systemctl(&["enable", SYSTEMD_UNIT]))
            .and_then(|_| systemctl(&["restart", SYSTEMD_UNIT]));
        if let Err(e) = started {
            let _ = systemctl(&["disable", SYSTEMD_UNIT]);
            let _ = std::fs::remove_file(&path);
            let _ = systemctl(&["daemon-reload"]);
            return Err(format!("Keeping a folder shared needs systemd's user services, which did not take it ({e})"));
        }
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::MetadataExt;
        let path = launch_agent_path().ok_or("cannot tell where your home folder is (HOME is not set)")?;
        let plist = launchd_plist(entry)?;
        write_own(&path, &plist)?;
        let uid = std::fs::metadata(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?.uid();
        let launchctl = |args: &[&str]| run_tool(launchctl_cmd(args), entry.prepare);
        // One loaded before (an earlier Keep sharing) goes first; none is fine.
        let _ = launchctl(&["bootout", &format!("gui/{uid}/{LAUNCHD_LABEL}")]);
        let path_text = path.to_string_lossy().into_owned();
        // Taking the old one away finishes a moment later.
        let mut tries = 0;
        loop {
            match launchctl(&["bootstrap", &format!("gui/{uid}"), &path_text]) {
                Ok(_) => return Ok(()),
                Err(_) if tries < 20 => {
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(250));
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&path);
                    return Err(format!("macOS did not take the login item ({e})"));
                }
            }
        }
    }
    #[cfg(windows)]
    {
        let line = windows_command_line(entry)?;
        registry::set(RUN_KEY, RUN_VALUE, &line)?;
        if let Err(e) = start_detached(entry) {
            let _ = registry::delete(RUN_KEY, RUN_VALUE);
            return Err(e);
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = entry;
        Err("keeping a folder shared at login is not available on this system".into())
    }
}

#[cfg(target_os = "macos")]
fn launchctl_cmd(args: &[&str]) -> Command {
    let mut c = Command::new("/bin/launchctl");
    c.args(args);
    c
}

/// Starts the server now, on its own (it goes on after PlanCraft quits).
#[cfg(windows)]
fn start_detached(entry: &Entry) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    let spawn = |flags: u32| {
        let mut c = Command::new(&entry.program);
        c.args(&entry.args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(flags);
        if entry.config.is_dir() {
            c.current_dir(&entry.config);
        }
        c.spawn()
    };
    let base = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP;
    // Out of PlanCraft's job, if it is in one that allows it, so closing PlanCraft leaves it be.
    spawn(base | CREATE_BREAKAWAY_FROM_JOB)
        .or_else(|_| spawn(base))
        .map(drop)
        .map_err(|e| format!("cannot start the server ({}): {e}", entry.program.display()))
}

/// Stops starting the server at login. It does not stop one running now: ask it to stop first
/// ([`crate::background::request_stop`], then [`crate::background::wait_stopped`]); on macOS,
/// one still running is sent the termination signal, which also stops it saving its plans, but
/// only for as long as launchd waits.
pub fn disable(prepare: Option<fn(&mut Command)>) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let Some(path) = systemd_unit_path() else { return Ok(()) };
        if std::fs::symlink_metadata(&path).is_err() {
            return Ok(());
        }
        let systemctl = |args: &[&str]| {
            let mut c = Command::new("systemctl");
            c.arg("--user").args(args);
            run_tool(c, prepare)
        };
        // Not enabled (or systemd gone): the file still goes.
        let _ = systemctl(&["disable", SYSTEMD_UNIT]);
        std::fs::remove_file(&path).map_err(|e| format!("cannot remove {}: {e}", path.display()))?;
        let _ = systemctl(&["daemon-reload"]);
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::MetadataExt;
        let Some(path) = launch_agent_path() else { return Ok(()) };
        let Ok(meta) = std::fs::symlink_metadata(&path) else { return Ok(()) };
        // Taken out of launchd too (a server still running gets the termination signal).
        let _ = run_tool(launchctl_cmd(&["bootout", &format!("gui/{}/{LAUNCHD_LABEL}", meta.uid())]), prepare);
        std::fs::remove_file(&path).map_err(|e| format!("cannot remove {}: {e}", path.display()))
    }
    #[cfg(windows)]
    {
        let _ = prepare;
        registry::delete(RUN_KEY, RUN_VALUE)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = prepare;
        Ok(())
    }
}

/// Whether the system starts the server at login.
pub fn enabled() -> bool {
    #[cfg(target_os = "linux")]
    {
        systemd_unit_path().is_some_and(|p| std::fs::symlink_metadata(p).is_ok())
    }
    #[cfg(target_os = "macos")]
    {
        launch_agent_path().is_some_and(|p| std::fs::symlink_metadata(p).is_ok())
    }
    #[cfg(windows)]
    {
        registry::get(RUN_KEY, RUN_VALUE).is_some()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        false
    }
}

/// The person's own registry values (`HKEY_CURRENT_USER`).
#[cfg(windows)]
#[allow(unsafe_code)]
mod registry {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn set(key: &str, name: &str, value: &str) -> Result<(), String> {
        let (k, n, v) = (wide(key), wide(name), wide(value));
        let bytes = u32::try_from(v.len().saturating_mul(2)).map_err(|_| "the command is too long".to_string())?;
        // SAFETY: every pointer is to a NUL-terminated UTF-16 buffer that lives across the call;
        // `bytes` is the size of `v` in bytes, NUL included, as the call requires for REG_SZ.
        let r = unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, k.as_ptr(), n.as_ptr(), REG_SZ, v.as_ptr().cast(), bytes) };
        if r == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!("cannot write to the registry ({})", std::io::Error::from_raw_os_error(r as i32)))
        }
    }

    pub fn delete(key: &str, name: &str) -> Result<(), String> {
        let (k, n) = (wide(key), wide(name));
        // SAFETY: both pointers are to NUL-terminated UTF-16 buffers that live across the call.
        let r = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, k.as_ptr(), n.as_ptr()) };
        // ERROR_FILE_NOT_FOUND (2): not there, which is what was wanted.
        if r == ERROR_SUCCESS || r == 2 {
            Ok(())
        } else {
            Err(format!("cannot change the registry ({})", std::io::Error::from_raw_os_error(r as i32)))
        }
    }

    pub fn get(key: &str, name: &str) -> Option<String> {
        let (k, n) = (wide(key), wide(name));
        let mut buf = vec![0u16; 8 * 1024];
        let mut size = u32::try_from(buf.len().saturating_mul(2)).unwrap_or(u32::MAX);
        // SAFETY: the key and name are NUL-terminated UTF-16 buffers; `buf` is writable for
        // `size` bytes, and the call writes at most that many and says how many in `size`.
        let r = unsafe {
            RegGetValueW(HKEY_CURRENT_USER, k.as_ptr(), n.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut size)
        };
        if r != ERROR_SUCCESS {
            return None;
        }
        let len = usize::try_from(size / 2).unwrap_or(0).min(buf.len());
        buf.truncate(len);
        while buf.last() == Some(&0) {
            buf.pop();
        }
        Some(String::from_utf16_lossy(&buf))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    fn entry(folder: &str) -> Entry {
        Entry {
            program: PathBuf::from("/opt/Plan Craft/bin/plancraft"),
            args: ["serve", "--background", "--config", "/home/ana/.config/plancraft/server", "--port", "47340", "--", folder]
                .into_iter()
                .map(OsString::from)
                .collect(),
            name: "Plans".into(),
            config: PathBuf::from("/home/ana/.config/plancraft/server"),
            prepare: None,
        }
    }

    /// systemd's own reading of a command line's quoted words (systemd.service › Command lines).
    fn systemd_read(line: &str) -> Vec<String> {
        let mut words = Vec::new();
        let mut it = line.chars().peekable();
        while let Some(c) = it.next() {
            match c {
                ' ' => {}
                '"' => {
                    let mut w = String::new();
                    while let Some(c) = it.next() {
                        match c {
                            '\\' => w.push(it.next().unwrap()),
                            '"' => break,
                            '%' => {
                                assert_eq!(it.next(), Some('%'), "a lone % is a specifier");
                                w.push('%');
                            }
                            '$' => {
                                assert_eq!(it.next(), Some('$'), "a lone $ is a variable");
                                w.push('$');
                            }
                            c => w.push(c),
                        }
                    }
                    words.push(w);
                }
                c => panic!("unquoted {c}"),
            }
        }
        words
    }

    #[test]
    fn the_systemd_unit_runs_exactly_the_command() {
        let odd = r#"/home/ana/Plans "100%" $HOME \n ; end\"#;
        let e = entry(odd);
        let unit = systemd_unit(&e).unwrap();
        let exec = unit.lines().find_map(|l| l.strip_prefix("ExecStart=")).unwrap();
        let mut want = vec!["/opt/Plan Craft/bin/plancraft".to_string()];
        want.extend(e.args.iter().map(|a| a.to_string_lossy().into_owned()));
        assert_eq!(systemd_read(exec), want);
        assert!(unit.contains("Restart=on-failure") && unit.contains("WantedBy=default.target"));
        // A server that cannot start (or a program removed) is not started every 30 s forever.
        assert!(unit.contains("StartLimitIntervalSec=600\nStartLimitBurst=5\n"), "{unit}");
        // A folder named with a specifier is not expanded in the description either.
        let mut e = entry("/p");
        e.name = "50% done\nExecStart=/bin/false".into();
        let unit = systemd_unit(&e).unwrap();
        assert!(unit.contains("Description=PlanCraft Server sharing 50%% doneExecStart=/bin/false\n"));
        assert_eq!(unit.lines().filter(|l| l.starts_with("ExecStart=")).count(), 1);
    }

    #[test]
    fn names_a_command_line_cannot_carry_are_refused() {
        assert!(systemd_unit(&entry("/home/ana/Plans\nExecStartPre=/bin/rm")).is_err());
        assert!(launchd_plist(&entry("/a\u{7}b")).is_err());
        assert!(windows_command_line(&entry("C:\\Plans %PATH%")).is_err());
    }

    #[test]
    fn the_launch_agent_lists_every_argument() {
        let e = entry("/Users/ana/R&D <plans> 'x'");
        let plist = launchd_plist(&e).unwrap();
        assert!(plist.contains("<string>/Users/ana/R&amp;D &lt;plans&gt; &apos;x&apos;</string>"), "{plist}");
        assert!(plist.contains(&format!("<string>{LAUNCHD_LABEL}</string>")));
        assert_eq!(plist.matches("<string>").count(), e.args.len() + 1 + 3);
        assert!(plist.contains("<key>SuccessfulExit</key>"));
        // Time to save the plans when launchd stops it.
        assert!(plist.contains("<key>ExitTimeOut</key>\n    <integer>60</integer>"), "{plist}");
    }

    /// The standard library's (and the C runtime's) reading of a Windows command line's
    /// arguments, after the program.
    fn windows_read(line: &str) -> Vec<String> {
        let mut args = Vec::new();
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            while i < chars.len() && chars[i] == ' ' {
                i += 1;
            }
            if i >= chars.len() {
                break;
            }
            let mut w = String::new();
            let mut quoted = false;
            while i < chars.len() && (quoted || chars[i] != ' ') {
                let mut slashes = 0;
                while i < chars.len() && chars[i] == '\\' {
                    slashes += 1;
                    i += 1;
                }
                if i < chars.len() && chars[i] == '"' {
                    w.extend(std::iter::repeat_n('\\', slashes / 2));
                    if slashes % 2 == 1 {
                        w.push('"');
                    } else {
                        quoted = !quoted;
                    }
                    i += 1;
                } else {
                    w.extend(std::iter::repeat_n('\\', slashes));
                    if i < chars.len() && (quoted || chars[i] != ' ') {
                        w.push(chars[i]);
                        i += 1;
                    }
                }
            }
            args.push(w);
        }
        args
    }

    #[test]
    fn the_windows_command_line_reads_back() {
        for folder in [r"C:\Users\Ana\Plans", r"C:\Plans with space\", r#"C:\odd "quoted" \\"#, r"\\server\share\Plans", ""] {
            let mut e = entry(folder);
            e.program = PathBuf::from(r"C:\Program Files\PlanCraft\plancraft.exe");
            let line = windows_command_line(&e).unwrap();
            let (program, rest) = line.strip_prefix('"').and_then(|l| l.split_once("\" ")).unwrap();
            assert_eq!(program, r"C:\Program Files\PlanCraft\plancraft.exe");
            let want: Vec<String> = e.args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
            assert_eq!(windows_read(rest), want, "{line}");
            // Plain words are not quoted, to keep the line short.
            assert!(rest.starts_with("serve --background --config "), "{line}");
        }
    }

    #[test]
    fn a_windows_command_line_too_long_to_run_at_login_is_refused() {
        let mut e = entry(&format!(r"C:\Users\Ana\{}", "Plans ".repeat(20)));
        e.program = PathBuf::from(r"C:\Program Files\PlanCraft\plancraft.exe");
        assert!(windows_command_line(&e).is_ok());
        let mut e = entry(&format!(r"C:\Users\Ana\{}", "Plans ".repeat(30)));
        e.program = PathBuf::from(r"C:\Program Files\PlanCraft\plancraft.exe");
        let why = windows_command_line(&e).unwrap_err();
        assert!(why.contains("too long") && why.contains("260"), "{why}");
    }

    #[test]
    fn a_program_in_the_temporary_folder_is_seen_by_any_name() {
        let temp = std::env::temp_dir();
        assert!(in_folder(&temp.join("Temp1_PlanCraft.zip").join("plancraft"), &temp));
        assert!(!in_folder(Path::new("/opt/plancraft/plancraft"), &temp));
        // By its real name too (macOS: /var is /private/var).
        let dir = tempfile::tempdir().unwrap();
        let real = plancraft_core::io::canonical(dir.path()).unwrap();
        assert!(in_folder(&real.join("plancraft"), &temp), "{} in {}", real.display(), temp.display());
        if cfg!(windows) {
            let upper = PathBuf::from(temp.to_string_lossy().to_uppercase()).join("plancraft.exe");
            assert!(in_folder(&upper, &temp));
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_entry_shares_the_folder_in_the_background() {
        let dir = tempfile::tempdir().unwrap();
        // Not in the temporary folder, which is refused.
        let program = Program { program: PathBuf::from("/opt/plancraft/plancraft"), args: vec!["serve".into()], prepare: None };
        let config = PathBuf::from("/home/ana/.config/plancraft/server");
        let e = Entry::new(&program, dir.path(), &config, 47341, false).unwrap();
        let args: Vec<String> = e.args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        let folder = plancraft_core::io::canonical(dir.path()).unwrap().to_string_lossy().into_owned();
        assert_eq!(
            args,
            ["serve", "--background", "--config", "/home/ana/.config/plancraft/server", "--port", "47341", "--no-announce", "--", &folder]
        );
        // A program in the temporary folder is gone by the next login.
        let temporary = Program { program: std::env::temp_dir().join("plancraft"), ..program };
        assert!(Entry::new(&temporary, dir.path(), &config, 47341, true).unwrap_err().contains("temporary"));
        assert!(Entry::new(&Program { program: PathBuf::from("plancraft"), ..temporary }, dir.path(), &config, 1, true).is_err());
    }
}
