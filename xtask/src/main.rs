//! PlanCraft build tasks.
//!
//! ```text
//! cargo xtask package <mac|linux>          build + package on this machine
//! cargo xtask dist <mac|linux|all> [--host ALIAS]
//!                                          package here, or on a build host over SSH,
//!                                          and collect the results in dist/
//! ```
//!
//! Build hosts are SSH aliases. Configure them in `dist-hosts.toml` (git-ignored):
//!
//! ```toml
//! mac = "stepcraft-mac"
//! windows = "win-build"
//! ```

mod linux;
mod mac;
mod remote;
mod util;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, bail};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Mac,
    Linux,
    Windows,
}

impl Target {
    fn parse(s: &str) -> Result<Target> {
        Ok(match s {
            "mac" | "macos" => Target::Mac,
            "linux" => Target::Linux,
            "windows" | "win" => Target::Windows,
            other => bail!("unknown target '{other}' (expected mac, linux or windows)"),
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Target::Mac => "mac",
            Target::Linux => "linux",
            Target::Windows => "windows",
        }
    }

    fn is_local(self) -> bool {
        match self {
            Target::Mac => cfg!(target_os = "macos"),
            Target::Linux => cfg!(target_os = "linux"),
            Target::Windows => cfg!(target_os = "windows"),
        }
    }
}

const USAGE: &str = "\
PlanCraft build tasks

USAGE:
    cargo xtask package <mac|linux>
        Build release binaries and package them on this machine.

    cargo xtask dist <mac|linux|all> [--host SSH_ALIAS]
        Package locally when the target matches this machine, otherwise on a
        build host over SSH. Results (with .sha256 files) land in dist/.
        'all' packages every target that is local or has a configured host.

Build hosts come from --host, PLANCRAFT_<TARGET>_HOST, or dist-hosts.toml:
    mac = \"stepcraft-mac\"
    windows = \"win-build\"
";

fn package(target: Target) -> Result<Vec<PathBuf>> {
    match target {
        Target::Mac => mac::package(),
        Target::Linux => linux::package(),
        Target::Windows => bail!("Windows packaging is not implemented yet"),
    }
}

fn dist(target: Target, host: Option<&str>) -> Result<Vec<PathBuf>> {
    match remote::host_for(target, host)? {
        Some(h) if host.is_some() || !target.is_local() => remote::dist(target, &h),
        _ if target.is_local() => package(target),
        _ => bail!(
            "no build host configured for {0}; pass --host, set PLANCRAFT_{1}_HOST, or add `{0} = \"ssh-alias\"` to dist-hosts.toml",
            target.name(),
            target.name().to_uppercase()
        ),
    }
}

fn real_main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut host: Option<String> = None;
    let mut positional = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--host" => host = Some(it.next().cloned().ok_or_else(|| anyhow::anyhow!("--host needs a value"))?),
            "-h" | "--help" | "help" => {
                print!("{USAGE}");
                return Ok(());
            }
            _ => positional.push(a.as_str()),
        }
    }
    let results = match positional.as_slice() {
        ["package", t] => package(Target::parse(t)?)?,
        ["dist", "all"] => {
            if host.is_some() {
                bail!("--host cannot be combined with 'all'");
            }
            let mut all = Vec::new();
            for t in [Target::Linux, Target::Mac, Target::Windows] {
                if t == Target::Windows {
                    eprintln!("==> skipping windows (not implemented yet)");
                    continue;
                }
                if t.is_local() || remote::host_for(t, None)?.is_some() {
                    all.extend(dist(t, None)?);
                } else {
                    eprintln!("==> skipping {} (no build host configured)", t.name());
                }
            }
            all
        }
        ["dist", t] => dist(Target::parse(t)?, host.as_deref())?,
        _ => {
            eprint!("{USAGE}");
            bail!("expected a command");
        }
    };
    eprintln!();
    eprintln!("Done:");
    for p in results {
        let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        eprintln!("  {} ({:.1} MB)", p.display(), size as f64 / 1_048_576.0);
    }
    Ok(())
}

fn main() -> ExitCode {
    match real_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
