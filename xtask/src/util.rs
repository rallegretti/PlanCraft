//! Shared helpers: paths, versions, running commands, checksums.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

/// Workspace root (the directory containing the top-level Cargo.toml).
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask lives inside the workspace").to_path_buf()
}

/// `[workspace.package] version` from the root Cargo.toml.
pub fn version() -> Result<String> {
    let text = std::fs::read_to_string(root().join("Cargo.toml")).context("reading Cargo.toml")?;
    let mut in_section = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line == "[workspace.package]";
            continue;
        }
        if in_section
            && let Some(rest) = line.strip_prefix("version")
            && let Some(v) = rest.trim().strip_prefix('=')
        {
            return Ok(v.trim().trim_matches('"').to_string());
        }
    }
    bail!("no [workspace.package] version in Cargo.toml")
}

/// Prints a progress line (stderr, so stdout stays machine-readable).
pub fn step(msg: impl AsRef<str>) {
    eprintln!("==> {}", msg.as_ref());
}

/// Runs a command, echoing it, and fails on a non-zero exit status.
pub fn run(cmd: &mut Command) -> Result<()> {
    eprintln!("    $ {}", describe(cmd));
    let status = cmd.status().with_context(|| format!("failed to start {}", describe(cmd)))?;
    if !status.success() {
        bail!("{} exited with {status}", describe(cmd));
    }
    Ok(())
}

/// Runs a command and returns its trimmed stdout.
pub fn output(cmd: &mut Command) -> Result<String> {
    let out = cmd.output().with_context(|| format!("failed to start {}", describe(cmd)))?;
    if !out.status.success() {
        bail!("{} exited with {}: {}", describe(cmd), out.status, String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn describe(cmd: &Command) -> String {
    let mut s = cmd.get_program().to_string_lossy().to_string();
    for a in cmd.get_args() {
        let a = a.to_string_lossy();
        if a.contains(' ') { s += &format!(" \"{a}\"") } else { s += &format!(" {a}") }
    }
    s
}

/// The cargo that invoked us (falls back to `cargo` on PATH).
pub fn cargo() -> Command {
    let mut c = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    c.current_dir(root());
    c
}

/// Builds the release binaries of the app and the standalone MCP server.
pub fn build_release(target: Option<&str>, envs: &[(&str, &str)]) -> Result<()> {
    let mut c = cargo();
    c.args(["build", "--release", "--locked", "-p", "plancraft-app", "-p", "plancraft-mcp"]);
    if let Some(t) = target {
        c.args(["--target", t]);
    }
    for (k, v) in envs {
        c.env(k, v);
    }
    run(&mut c)
}

/// Recreates an empty directory.
pub fn fresh_dir(path: &Path) -> Result<()> {
    if path.exists() {
        std::fs::remove_dir_all(path).with_context(|| format!("removing {}", path.display()))?;
    }
    std::fs::create_dir_all(path).with_context(|| format!("creating {}", path.display()))
}

/// Copies a file, creating parent directories and keeping it executable on Unix when asked.
pub fn copy(from: &Path, to: &Path, executable: bool) -> Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(from, to).with_context(|| format!("copying {} to {}", from.display(), to.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if executable { 0o755 } else { 0o644 };
        std::fs::set_permissions(to, std::fs::Permissions::from_mode(mode))?;
    }
    let _ = executable;
    Ok(())
}

/// Writes `<file>.sha256` next to `file` in the usual `sha256sum` format.
pub fn write_checksum(file: &Path) -> Result<PathBuf> {
    let bytes = std::fs::read(file)?;
    let digest = Sha256::digest(&bytes);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    let name = file.file_name().unwrap().to_string_lossy();
    let sum_path = file.with_file_name(format!("{name}.sha256"));
    std::fs::write(&sum_path, format!("{hex}  {name}\n"))?;
    Ok(sum_path)
}

/// Announces a finished artifact on stdout so a remote driver can fetch it.
pub fn artifact(path: &Path) {
    let rel = path.strip_prefix(root()).unwrap_or(path);
    println!("ARTIFACT: {}", rel.to_string_lossy().replace('\\', "/"));
}

/// Renders the app icon at `size` px using the freshly built app binary.
pub fn render_icon(app_binary: &Path, out: &Path, size: u32) -> Result<()> {
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    output(Command::new(app_binary).arg("--write-icon").arg(out).arg(size.to_string()))?;
    Ok(())
}
