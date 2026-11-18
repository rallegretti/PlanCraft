#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! The `plancraft-server` command line: managing users, and serving.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

fn run(config: &std::path::Path, args: &[&str], stdin: &str) -> (bool, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_plancraft-server"))
        .args(args)
        .arg("--config")
        .arg(config)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn users_are_managed_from_the_command_line() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config");
    let (ok, out, _) = run(&config, &["user", "list"], "");
    assert!(ok && out.contains("No users yet"));
    let (ok, _, err) = run(&config, &["user", "add", "ana", "--role", "admin", "--password-stdin"], "correct horse\n");
    assert!(ok, "{err}");
    let (ok, _, err) = run(&config, &["user", "add", "ben", "--password-stdin"], "short\n");
    assert!(!ok && err.contains("8 characters"), "{err}");
    let (ok, _, err) = run(&config, &["user", "add", "ben", "--role", "boss", "--password-stdin"], "correct horse\n");
    assert!(!ok && err.contains("not a role"), "{err}");
    let (ok, _, _) = run(&config, &["user", "add", "ben", "--password-stdin"], "correct horse\n");
    assert!(ok);
    let (ok, _, _) = run(&config, &["user", "role", "ben", "none", "--plan", "Secret.plancraft"], "");
    assert!(ok);
    let (_, out, _) = run(&config, &["user", "list"], "");
    assert!(out.contains("ana  admin"), "{out}");
    assert!(out.contains("ben  editor  (Secret.plancraft: none)"), "{out}");
    let (ok, _, _) = run(&config, &["user", "passwd", "ben", "--password-stdin"], "battery staple\n");
    assert!(ok);
    let (ok, _, _) = run(&config, &["user", "remove", "ben"], "");
    assert!(ok);
    let (ok, _, err) = run(&config, &["user", "remove", "ben"], "");
    assert!(!ok && err.contains("no user"), "{err}");
    let users = std::fs::read_to_string(config.join("users.json")).unwrap();
    assert!(!users.contains("correct horse") && !users.contains("battery staple"));
}

#[test]
fn the_fingerprint_stays_the_same() {
    let dir = tempfile::tempdir().unwrap();
    let (ok, a, _) = run(dir.path(), &["fingerprint"], "");
    let (_, b, _) = run(dir.path(), &["fingerprint"], "");
    assert!(ok);
    assert_eq!(a, b);
    assert_eq!(a.trim().split(' ').count(), 8);
}

#[test]
fn serving_needs_a_user_then_listens() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config");
    let folder = dir.path().join("shared");
    std::fs::create_dir(&folder).unwrap();
    let folder_arg = folder.to_str().unwrap();
    let (ok, _, err) = run(&config, &[folder_arg, "--port", "0", "--bind", "127.0.0.1"], "");
    assert!(!ok && err.contains("user add"), "{err}");
    let (ok, _, err) = run(&config, &[folder_arg, "--port", "nope"], "");
    assert!(!ok && err.contains("not a port"), "{err}");
    run(&config, &["user", "add", "ana", "--password-stdin"], "correct horse\n");

    let mut child = Command::new(env!("CARGO_BIN_EXE_plancraft-server"))
        .args([folder_arg, "--port", "0", "--bind", "127.0.0.1", "--config"])
        .arg(&config)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
    let listening = lines.by_ref().take(5).map(Result::unwrap).find(|l| l.contains("Listening on")).unwrap();
    let addr: std::net::SocketAddr = listening.rsplit(' ').next().unwrap().parse().unwrap();
    let mut c = plancraft_server::client::Client::connect(addr, &plancraft_server::wire::Trust::FirstUse, "test").unwrap();
    assert_eq!(c.login("ana", "correct horse").unwrap(), "ana");
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn help_says_what_it_does() {
    let dir = tempfile::tempdir().unwrap();
    let (ok, out, _) = run(dir.path(), &["--help"], "");
    assert!(ok && out.contains("plancraft-server FOLDER"));
    let (ok, _, err) = run(dir.path(), &["--bogus"], "");
    assert!(!ok && err.contains("unknown option"));
}
