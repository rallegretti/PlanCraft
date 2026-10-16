//! `cargo xtask ci`: the same quality gates GitHub Actions runs, on this machine.
//!
//! Formatting, clippy with warnings denied (including the never-crash lints), the whole test
//! suite, the scheduler speed gate in release mode, and `cargo deny` (licences, advisories,
//! banned crates, sources).

use std::process::Command;

use anyhow::{Result, bail};

use crate::util::{self, run, step};

pub fn ci() -> Result<()> {
    let root = util::root();
    let cargo = || {
        let mut c = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
        c.current_dir(&root);
        c
    };
    step("format (cargo fmt --check)");
    run(cargo().args(["fmt", "--all", "--", "--check"]))?;
    step("clippy, warnings denied");
    run(cargo().args(["clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"]))?;
    step("tests");
    run(cargo().args(["test", "--workspace", "--locked"]))?;
    step("scheduler speed gate (release)");
    run(cargo().args(["test", "--release", "--locked", "-p", "plancraft-core", "--test", "speed"]))?;
    step("cargo deny");
    if Command::new("cargo-deny").arg("--version").output().is_err() {
        bail!("cargo-deny is not installed; install it with: cargo install --locked cargo-deny");
    }
    run(cargo().args(["deny", "--locked", "check"]))?;
    eprintln!("\nAll checks passed.");
    Ok(())
}
