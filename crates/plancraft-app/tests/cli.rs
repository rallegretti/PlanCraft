//! The `plancraft` binary's command line.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

/// A file name that is not valid UTF-8 (a Latin-1 name from a file manager) must not crash the
/// app before any of its guards run (regression: `std::env::args` panicked).
#[cfg(unix)]
#[test]
fn a_file_name_that_is_not_utf8_does_not_crash() {
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::OsStr::from_bytes(b"caf\xe9.plancraft");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_plancraft")).arg("--version").arg(name).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("plancraft "));
}
