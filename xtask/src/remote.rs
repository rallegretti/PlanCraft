//! Building on another machine over SSH.
//!
//! The current source (HEAD plus uncommitted changes to tracked files) is
//! streamed to `~/Builds/PlanCraft` on the host with `git archive | ssh … tar`,
//! `cargo xtask package <target>` runs there, and every artifact it announces is
//! copied back into the local `dist/` directory with `scp`.
//!
//! Hosts are SSH aliases from `~/.ssh/config`, chosen with `--host`, the
//! `PLANCRAFT_<TARGET>_HOST` environment variable, or `dist-hosts.toml`.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::Target;
use crate::util::{self, output, step};

/// Remote working directory, relative to the SSH user's home directory.
const REMOTE_DIR: &str = "Builds/PlanCraft";

/// Looks up the SSH host for a target.
pub fn host_for(target: Target, explicit: Option<&str>) -> Result<Option<String>> {
    if let Some(h) = explicit {
        return Ok(Some(h.to_string()));
    }
    let key = target.name();
    if let Ok(h) = std::env::var(format!("PLANCRAFT_{}_HOST", key.to_uppercase())) {
        return Ok(Some(h));
    }
    let file = util::root().join("dist-hosts.toml");
    if let Ok(text) = std::fs::read_to_string(&file) {
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if let Some((k, v)) = line.split_once('=')
                && k.trim() == key
            {
                return Ok(Some(v.trim().trim_matches('"').to_string()));
            }
        }
    }
    Ok(None)
}

fn ssh(host: &str) -> Command {
    let mut c = Command::new("ssh");
    c.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", host]);
    c
}

/// Wraps a PowerShell script as `powershell -EncodedCommand …` so no quoting survives
/// the trip through ssh and the Windows command line.
fn powershell(script: &str) -> String {
    let script = format!("$ProgressPreference = 'SilentlyContinue'\n{script}");
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    format!("powershell -NoProfile -NonInteractive -EncodedCommand {}", base64(&utf16))
}

fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The git tree to send: HEAD, or a snapshot commit that includes uncommitted changes.
fn source_revision() -> Result<String> {
    let root = util::root();
    let untracked = output(Command::new("git").current_dir(&root).args(["ls-files", "--others", "--exclude-standard"]))?;
    if !untracked.is_empty() {
        eprintln!("    warning: untracked files are not sent to the build host:");
        for f in untracked.lines().take(10) {
            eprintln!("      {f}");
        }
    }
    let snapshot = output(Command::new("git").current_dir(&root).args(["stash", "create"]))?;
    if snapshot.is_empty() {
        Ok("HEAD".into())
    } else {
        eprintln!("    including uncommitted changes to tracked files");
        Ok(snapshot)
    }
}

/// Packages `target` on `host` and copies the artifacts into `dist/`.
pub fn dist(target: Target, host: &str) -> Result<Vec<PathBuf>> {
    let root = util::root();
    let windows_host = target == Target::Windows;

    step(format!("checking {host}"));
    let reply = output(ssh(host).arg("echo ok")).with_context(|| format!("cannot reach {host} over SSH"))?;
    if reply.trim() != "ok" {
        bail!("unexpected reply from {host}: {reply}");
    }

    step(format!("sending source to {host}:~/{REMOTE_DIR}"));
    let rev = source_revision()?;
    let extract = if windows_host {
        powershell(&format!(
            "$ErrorActionPreference = 'Stop'
             New-Item -ItemType Directory -Force -Path '{REMOTE_DIR}' | Out-Null
             Get-ChildItem -Force '{REMOTE_DIR}' | Where-Object {{ $_.Name -notin @('target','dist') }} | Remove-Item -Recurse -Force
             tar -x -f - -C '{REMOTE_DIR}'
             exit $LASTEXITCODE"
        ))
    } else {
        format!(
            "set -e; mkdir -p {REMOTE_DIR}; cd {REMOTE_DIR}; \
             find . -mindepth 1 -maxdepth 1 ! -name target ! -name dist -exec rm -rf {{}} +; tar -x -f -"
        )
    };
    let mut archive = Command::new("git")
        .current_dir(&root)
        .args(["archive", "--format=tar", &rev])
        .stdout(Stdio::piped())
        .spawn()
        .context("running git archive")?;
    let mut upload = ssh(host).arg(extract).stdin(Stdio::piped()).spawn().context("starting ssh")?;
    {
        let mut from = archive.stdout.take().unwrap();
        let mut to = upload.stdin.take().unwrap();
        std::io::copy(&mut from, &mut to).context("streaming source to the build host")?;
        to.flush()?;
    }
    if !archive.wait()?.success() || !upload.wait()?.success() {
        bail!("sending the source to {host} failed");
    }

    step(format!("packaging on {host}"));
    let build = if windows_host {
        // Merge stderr into stdout as plain text (PowerShell 5.1 otherwise wraps it as errors).
        powershell(&format!(
            "Set-Location '{REMOTE_DIR}'
             & \"$env:USERPROFILE\\.cargo\\bin\\cargo.exe\" xtask package {} 2>&1 | ForEach-Object {{ if ($_ -is [System.Management.Automation.ErrorRecord]) {{ \"$($_.TargetObject)\" }} else {{ \"$_\" }} }}
             exit $LASTEXITCODE",
            target.name()
        ))
    } else {
        format!("export PATH=\"$HOME/.cargo/bin:$PATH\"; cd {REMOTE_DIR} && cargo xtask package {}", target.name())
    };
    let mut child = ssh(host).arg(build).stdout(Stdio::piped()).spawn().context("starting remote build")?;
    let mut artifacts = Vec::new();
    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        let line = line?;
        match line.strip_prefix("ARTIFACT: ") {
            Some(path) => artifacts.push(path.trim().to_string()),
            None => println!("{line}"),
        }
    }
    if !child.wait()?.success() {
        bail!("packaging failed on {host}");
    }
    if artifacts.is_empty() {
        bail!("{host} finished but reported no artifacts");
    }

    step("fetching artifacts");
    let local_dir = root.join("dist");
    std::fs::create_dir_all(&local_dir)?;
    let mut fetched = Vec::new();
    for a in artifacts {
        let name = a.rsplit('/').next().unwrap_or(&a).to_string();
        let local = local_dir.join(&name);
        util::run(Command::new("scp").args(["-q", "-o", "BatchMode=yes"]).arg(format!("{host}:{REMOTE_DIR}/{a}")).arg(&local))?;
        fetched.push(local);
    }
    Ok(fetched)
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(super::base64(b""), "");
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foo"), "Zm9v");
        assert_eq!(super::base64(b"foobar"), "Zm9vYmFy");
    }
}
