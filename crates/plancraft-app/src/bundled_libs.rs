//! Linux packages carry a few window-system libraries that the app loads at startup and that a
//! fresh desktop may lack (keyboard maps, cursors, screens, input devices), each in its own folder
//! under `lib/plancraft/` next to `bin/` (see `BUNDLED_LIBS` in `xtask/src/linux.rs`). They only
//! fill gaps: when this system lacks one, the app restarts itself once with just the folders of
//! the missing ones on `LD_LIBRARY_PATH`. A library the system has is always the system's own
//! copy, which matches its keyboard data, display server and drivers.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

/// Set on the restarted process, so it never restarts again.
const RESTARTED: &str = "PLANCRAFT_BUNDLED_LIBS";
/// Bounds on reading the loader's configuration (`include` lines can nest).
const MAX_CONF_DEPTH: usize = 4;
const MAX_CONF_FILES: usize = 256;

/// Restarts the app with the bundled libraries this system lacks; returns (doing nothing) when
/// none are missing, when this is not a package with bundled libraries, or if restarting fails.
pub fn use_where_missing() {
    if std::env::var_os(RESTARTED).is_some() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(bundle) = exe.parent().and_then(Path::parent).map(|p| p.join("lib/plancraft")) else { return };
    let missing = missing_dirs(&bundle, &system_dirs());
    if missing.is_empty() {
        return;
    }
    let mut paths = missing;
    if let Some(old) = std::env::var_os("LD_LIBRARY_PATH") {
        // An empty entry would mean the current directory; leave those out.
        paths.extend(std::env::split_paths(&old).filter(|p| !p.as_os_str().is_empty()));
    }
    let Ok(joined) = std::env::join_paths(paths) else { return };
    let mut args = std::env::args_os();
    let mut cmd = std::process::Command::new(&exe);
    if let Some(arg0) = args.next() {
        cmd.arg0(arg0);
    }
    cmd.args(args).env("LD_LIBRARY_PATH", joined).env(RESTARTED, "1");
    // `exec` only returns when it fails; the app then starts without the bundled libraries.
    let err = cmd.exec();
    eprintln!("plancraft: could not restart with the bundled libraries: {err}");
}

/// The folders under `bundle` holding a library that none of `system` has.
fn missing_dirs(bundle: &Path, system: &[PathBuf]) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(bundle) else { return Vec::new() };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|dir| {
            std::fs::read_dir(dir)
                .map(|libs| libs.flatten().any(|lib| !system.iter().any(|d| is_native_library(&d.join(lib.file_name())))))
                .unwrap_or(false)
        })
        .collect();
    out.sort();
    out
}

/// Where the dynamic loader finds libraries: `LD_LIBRARY_PATH`, the folders in
/// `/etc/ld.so.conf` (with its `include`s) and the standard folders.
fn system_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("LD_LIBRARY_PATH")
        .map(|v| std::env::split_paths(&v).filter(|p| !p.as_os_str().is_empty()).collect())
        .unwrap_or_default();
    let mut files = 0;
    conf_dirs(Path::new("/etc/ld.so.conf"), 0, &mut files, &mut dirs);
    for d in [
        "/lib/x86_64-linux-gnu",
        "/usr/lib/x86_64-linux-gnu",
        "/lib/aarch64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
        "/lib64",
        "/usr/lib64",
        "/lib",
        "/usr/lib",
    ] {
        dirs.push(PathBuf::from(d));
    }
    dirs
}

/// Adds the folders listed in a loader configuration file (`include PATTERN` lines followed).
fn conf_dirs(path: &Path, depth: usize, files: &mut usize, out: &mut Vec<PathBuf>) {
    *files += 1;
    if depth > MAX_CONF_DEPTH || *files > MAX_CONF_FILES {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else { return };
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if let Some(pattern) = line.strip_prefix("include").filter(|r| r.starts_with(char::is_whitespace)) {
            for file in expand(path, pattern.trim()) {
                conf_dirs(&file, depth + 1, files, out);
            }
        } else {
            // Folders separated by spaces, tabs or commas; an old `DIR=TYPE` form drops the type.
            for dir in line.split(|c: char| c.is_whitespace() || c == ',').filter(|s| !s.is_empty()) {
                out.push(PathBuf::from(dir.split('=').next().unwrap_or(dir)));
            }
        }
    }
}

/// The files an `include` pattern names: a path, or a folder with one `*` in the file name
/// (`/etc/ld.so.conf.d/*.conf`), sorted. A relative pattern is relative to the including file.
fn expand(conf: &Path, pattern: &str) -> Vec<PathBuf> {
    let pattern = conf.parent().map(|dir| dir.join(pattern)).unwrap_or_else(|| PathBuf::from(pattern));
    let name = pattern.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let Some((prefix, suffix)) = name.split_once('*') else { return vec![pattern] };
    let Some(dir) = pattern.parent() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_name().to_str().is_some_and(|n| n.starts_with(prefix) && n.ends_with(suffix)))
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

/// Whether `path` is a shared library this process could load: 64-bit ELF for this machine (a
/// 32-bit copy in `/usr/lib` on a multilib system does not count).
fn is_native_library(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 20];
    let Ok(mut file) = std::fs::File::open(path) else { return false };
    if file.read_exact(&mut head).is_err() {
        return false;
    }
    let machine: u16 = if cfg!(target_arch = "aarch64") { 0xB7 } else { 0x3E };
    head.starts_with(b"\x7fELF")
        && head.get(4) == Some(&2)
        && head.get(5) == Some(&1)
        && head.get(18..20) == Some(machine.to_le_bytes().as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elf(machine: u16, class: u8) -> Vec<u8> {
        let mut b = vec![0u8; 64];
        b[..4].copy_from_slice(b"\x7fELF");
        b[4] = class;
        b[5] = 1;
        b[18..20].copy_from_slice(&machine.to_le_bytes());
        b
    }

    #[test]
    fn only_folders_of_libraries_the_system_lacks_are_used() {
        let root = std::env::temp_dir().join(format!("plancraft-bundled-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (bundle, system, multilib) = (root.join("lib/plancraft"), root.join("system"), root.join("lib32"));
        let native = if cfg!(target_arch = "aarch64") { 0xB7 } else { 0x3E };
        for name in ["libhave.so.1", "libmissing.so.2", "libonly32.so.0"] {
            let stem = name.split('.').next().unwrap();
            std::fs::create_dir_all(bundle.join(stem)).unwrap();
            std::fs::write(bundle.join(stem).join(name), elf(native, 2)).unwrap();
        }
        std::fs::create_dir_all(&system).unwrap();
        std::fs::create_dir_all(&multilib).unwrap();
        std::fs::write(system.join("libhave.so.1"), elf(native, 2)).unwrap();
        // A 32-bit copy and a file that is not a library do not count.
        std::fs::write(multilib.join("libonly32.so.0"), elf(0x03, 1)).unwrap();
        std::fs::write(system.join("libmissing.so.2"), b"not a library").unwrap();

        let dirs = missing_dirs(&bundle, &[system.clone(), multilib.clone()]);
        assert_eq!(dirs, vec![bundle.join("libmissing"), bundle.join("libonly32")]);
        // Nothing missing, or no bundle at all: no restart.
        assert!(missing_dirs(&bundle, std::slice::from_ref(&bundle.join("libhave"))).len() == 2);
        assert!(missing_dirs(&root.join("nowhere"), &[system]).is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn loader_configuration_is_followed() {
        let root = std::env::temp_dir().join(format!("plancraft-ldconf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("conf.d")).unwrap();
        std::fs::write(root.join("ld.so.conf"), "# comment\ninclude conf.d/*.conf\n/opt/a, /opt/b\n/opt/old=libc6\n").unwrap();
        std::fs::write(root.join("conf.d/x86.conf"), "/usr/local/lib # local\n").unwrap();
        std::fs::write(root.join("conf.d/skip.txt"), "/not/this\n").unwrap();
        // An include loop stops at the depth bound.
        std::fs::write(root.join("conf.d/loop.conf"), "include ../ld.so.conf\n").unwrap();
        let mut out = Vec::new();
        let mut files = 0;
        conf_dirs(&root.join("ld.so.conf"), 0, &mut files, &mut out);
        for d in ["/usr/local/lib", "/opt/a", "/opt/b", "/opt/old"] {
            assert!(out.contains(&PathBuf::from(d)), "{d} in {out:?}");
        }
        assert!(!out.contains(&PathBuf::from("/not/this")));
        assert!(files <= MAX_CONF_FILES);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
