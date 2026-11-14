//! Linux packages carry a few window-system libraries that the app loads at startup and that a
//! fresh desktop may lack (keyboard maps, cursors, screens, input devices), each in its own folder
//! under `lib/plancraft/` next to `bin/` (see `BUNDLED_LIBS` in `xtask/src/linux.rs`). They only
//! fill gaps: when this system lacks one, the app restarts itself once with just the folders of
//! the missing ones on `LD_LIBRARY_PATH`. A library the system has is always the system's own
//! copy, which matches its keyboard data, display server and drivers.
//!
//! The folders are trusted only when they look exactly as the packages make them: each holds one
//! of the known libraries and nothing else, and they and `lib/` are owned by root or by the
//! binary's owner, never writable by everyone, and writable by a group only when it is the
//! binary's own group (whose members could replace the binary anyway). So a binary run from a
//! shared folder like `/tmp` never loads libraries another user planted next to it.

use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Set on the restarted process, so it never restarts again.
const RESTARTED: &str = "PLANCRAFT_BUNDLED_LIBS";
/// The `LD_LIBRARY_PATH` the app was started with (absent if it had none), for child processes.
const ORIGINAL: &str = "PLANCRAFT_ORIGINAL_LD_LIBRARY_PATH";
/// The libraries the packages carry, each in a folder named after it without `.so.N` (keep in
/// step with `BUNDLED_LIBS` in `xtask/src/linux.rs`).
const KNOWN: [&str; 7] = [
    "libxkbcommon.so.0",
    "libxkbcommon-x11.so.0",
    "libxcb-xkb.so.1",
    "libXcursor.so.1",
    "libXi.so.6",
    "libXrender.so.1",
    "libwayland-egl.so.1",
];
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
    let Ok(owner) = std::fs::metadata(&exe).map(|m| Owner { uid: m.uid(), gid: m.gid() }) else { return };
    let Some(lib) = exe.parent().and_then(Path::parent).map(|p| p.join("lib")) else { return };
    let missing = missing_dirs(&lib.join("plancraft"), &system_dirs(), owner);
    if missing.is_empty() {
        return;
    }
    if missing.is_empty() || !trusted(&lib, owner) {
        return;
    }
    let old = std::env::var_os("LD_LIBRARY_PATH");
    let mut paths = missing;
    if let Some(old) = &old {
        // An empty entry would mean the current directory; leave those out.
        paths.extend(std::env::split_paths(old).filter(|p| !p.as_os_str().is_empty()));
    }
    let Ok(joined) = std::env::join_paths(paths) else { return };
    let mut args = std::env::args_os();
    let mut cmd = Command::new(&exe);
    if let Some(arg0) = args.next() {
        cmd.arg0(arg0);
    }
    cmd.args(args).env("LD_LIBRARY_PATH", joined).env(RESTARTED, "1");
    if let Some(old) = old {
        cmd.env(ORIGINAL, old);
    }
    // `exec` only returns when it fails; the app then starts without the bundled libraries.
    let err = cmd.exec();
    eprintln!("plancraft: could not restart with the bundled libraries: {err}");
}

/// Gives a child process the environment the app was started with: the bundled libraries are
/// for PlanCraft only, and a PlanCraft started from it checks for itself.
pub fn clean_child(cmd: &mut Command) {
    if std::env::var_os(RESTARTED).is_none() {
        return;
    }
    match std::env::var_os(ORIGINAL) {
        Some(old) => cmd.env("LD_LIBRARY_PATH", old),
        None => cmd.env_remove("LD_LIBRARY_PATH"),
    };
    cmd.env_remove(RESTARTED).env_remove(ORIGINAL);
}

/// Who owns the binary.
#[derive(Clone, Copy)]
struct Owner {
    uid: u32,
    gid: u32,
}

/// Whether `path` is owned by root or the binary's owner, is not writable by everyone, and is
/// writable by its group only when that is the binary's group.
fn safe(path: &Path, owner: Owner) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|m| (m.uid() == 0 || m.uid() == owner.uid) && m.mode() & 0o002 == 0 && (m.mode() & 0o020 == 0 || m.gid() == owner.gid))
}

/// Whether `lib/` and `lib/plancraft/` can be trusted (see the module notes).
fn trusted(lib: &Path, owner: Owner) -> bool {
    safe(lib, owner) && safe(&lib.join("plancraft"), owner)
}

/// The folders `bundle/<name>/` of known libraries that none of `system` has. A folder counts only
/// when it holds exactly its one library, and both are owned by root or `owner` and writable by no
/// one else (see `safe`).
fn missing_dirs(bundle: &Path, system: &[PathBuf], owner: Owner) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for soname in KNOWN {
        if system.iter().any(|d| is_native_library(&d.join(soname))) {
            continue;
        }
        let dir = bundle.join(soname.split(".so").next().unwrap_or(soname));
        let only_it = std::fs::read_dir(&dir).is_ok_and(|entries| {
            let names: Vec<_> = entries.flatten().map(|e| e.file_name()).collect();
            names.len() == 1 && names.first().is_some_and(|n| n == soname)
        });
        if only_it && safe(&dir, owner) && safe(&dir.join(soname), owner) && is_native_library(&dir.join(soname)) {
            out.push(dir);
        }
    }
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
    // The loader's built-in folders: on Debian-style systems the multiarch folders, `/lib` and
    // `/usr/lib` (never `/lib64`); on others `/lib64` and `/usr/lib64`, then `/lib` and `/usr/lib`.
    let multiarch: &[&str] = if cfg!(target_arch = "aarch64") {
        &["/lib/aarch64-linux-gnu", "/usr/lib/aarch64-linux-gnu"]
    } else {
        &["/lib/x86_64-linux-gnu", "/usr/lib/x86_64-linux-gnu"]
    };
    let builtin: &[&str] = if multiarch.iter().any(|d| Path::new(d).is_dir()) { multiarch } else { &["/lib64", "/usr/lib64"] };
    dirs.extend(builtin.iter().chain(&["/lib", "/usr/lib"]).map(PathBuf::from));
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

    fn put(dir: &Path, name: &str, bytes: &[u8]) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), bytes).unwrap();
    }

    fn mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn only_trusted_folders_of_libraries_the_system_lacks_are_used() {
        let root = std::env::temp_dir().join(format!("plancraft-bundled-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (lib, system, multilib) = (root.join("lib"), root.join("system"), root.join("lib32"));
        let bundle = lib.join("plancraft");
        let native = if cfg!(target_arch = "aarch64") { 0xB7 } else { 0x3E };
        for soname in ["libXi.so.6", "libXcursor.so.1", "libxkbcommon.so.0", "libXrender.so.1"] {
            put(&bundle.join(soname.split(".so").next().unwrap()), soname, &elf(native, 2));
        }
        put(&system, "libXi.so.6", &elf(native, 2));
        // A 32-bit copy and a file that is not a library do not count as the system having it.
        put(&multilib, "libXcursor.so.1", &elf(0x03, 1));
        put(&system, "libxkbcommon.so.0", b"not a library");
        let m = std::fs::metadata(&root).unwrap();
        let owner = Owner { uid: m.uid(), gid: m.gid() };
        let stranger = Owner { uid: m.uid().wrapping_add(1), gid: m.gid().wrapping_add(1) };
        let dirs = vec![system.clone(), multilib.clone()];
        let expected = vec![bundle.join("libXcursor"), bundle.join("libXrender"), bundle.join("libxkbcommon")];
        assert_eq!(missing_dirs(&bundle, &dirs, owner), expected);
        assert!(trusted(&lib, owner));

        // Planted files are never used: an unknown library, an extra file in a known folder, a
        // folder others can write to, or a folder owned by someone else.
        put(&bundle.join("evil"), "libnot-on-this-system.so.9", &elf(native, 2));
        put(&bundle.join("libXrender"), "libgcc_s.so.1", &elf(native, 2));
        mode(&bundle.join("libXcursor"), 0o757);
        let left = missing_dirs(&bundle, &dirs, owner);
        assert_eq!(left, vec![bundle.join("libxkbcommon")]);
        assert!(missing_dirs(&bundle, &dirs, stranger).is_empty() || owner.uid == 0);
        // Writable by the binary's own group (a umask of 002) is fine; by another group, not.
        mode(&bundle.join("libxkbcommon"), 0o775);
        assert_eq!(missing_dirs(&bundle, &dirs, owner), vec![bundle.join("libxkbcommon")]);
        assert!(missing_dirs(&bundle, &dirs, Owner { gid: owner.gid.wrapping_add(1), ..owner }).is_empty());
        // `lib/` or `lib/plancraft/` writable by others: nothing from the bundle is trusted.
        mode(&lib, 0o777);
        assert!(!trusted(&lib, owner));
        mode(&lib, 0o755);
        mode(&bundle, 0o757);
        assert!(!trusted(&lib, owner));
        mode(&bundle, 0o755);

        // No bundle at all: no restart.
        assert!(missing_dirs(&root.join("nowhere"), &dirs, owner).is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn child_processes_get_the_original_environment() {
        // Not restarted (as in tests): children are left alone.
        let mut cmd = Command::new("true");
        clean_child(&mut cmd);
        assert_eq!(cmd.get_envs().count(), 0);
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
