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

/// Regression: `plancraft-server user add` while PlanCraft's *Share Folder* was changing the
/// users file: whichever saved last lost the other's change. Changes now take turns.
#[test]
fn the_command_line_waits_for_a_change_made_elsewhere() {
    use plancraft_server::protocol::Role;
    use plancraft_server::users::{NewPassword, Users};
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config");
    let path = plancraft_server::server::users_path(&config);
    let password = NewPassword::new("correct horse").unwrap();
    // The command line, waiting for its password.
    let mut cli = Command::new(env!("CARGO_BIN_EXE_plancraft-server"))
        .args(["user", "add", "ben", "--password-stdin", "--config"])
        .arg(&config)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    Users::change(&path, |u| {
        // While PlanCraft holds the users file, the command line gets its password, hashes it
        // and comes to the file: it must wait, not save over this change.
        cli.stdin.take().unwrap().write_all(b"correct horse\n").unwrap();
        std::thread::sleep(std::time::Duration::from_secs(3));
        assert!(cli.try_wait().unwrap().is_none(), "the command line finished while the file was held");
        u.add_with("Ana", &password, Role::Editor)
    })
    .unwrap();
    let out = cli.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let names: Vec<String> = Users::load(&path).unwrap().list().iter().map(|u| u.name.clone()).collect();
    assert_eq!(names, ["Ana", "ben"]);
}

/// After `--`, the name is the folder to share, even one that reads like a command; and why the
/// server stopped is one line, whatever the folder's name holds.
#[test]
fn a_name_after_dashes_is_always_the_folder() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config");
    let serve = |name: &str| {
        let out = Command::new(env!("CARGO_BIN_EXE_plancraft-server"))
            .current_dir(dir.path())
            .arg("--config")
            .arg(&config)
            .args(["--port", "0", "--bind", "127.0.0.1", "--no-announce", "--", name])
            .output()
            .unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    };
    for name in ["--help", "help", "user", "fingerprint"] {
        let (ok, out, err) = serve(name);
        assert!(!ok && !out.contains("Usage") && err.contains("cannot open the folder"), "{name}: {out} {err}");
    }
    #[cfg(unix)]
    {
        let (ok, _, err) = serve("two\nlines");
        assert!(!ok && err.trim_end().lines().count() == 1 && err.starts_with("plancraft-server: "), "{err:?}");
    }
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

/// `--background` (Keep sharing at login): one server per private folder, saying where it
/// listens; `stop` stops it with every plan saved; and why one could not start is in its log
/// file, which a service's standard error is not.
#[test]
fn a_background_server_says_where_it_is_and_stops_when_asked() {
    use plancraft_server::background::{self, Background};
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config");
    let folder = dir.path().join("shared");
    std::fs::create_dir(&folder).unwrap();
    let folder_arg = folder.to_str().unwrap();
    // No `--`: the helper puts `--config` after these.
    let serve = ["--background", "--port", "0", "--bind", "127.0.0.1", "--no-announce", folder_arg];

    // Nobody may sign in yet: it cannot start, and its log says why.
    let (ok, _, _) = run(&config, &serve, "");
    assert!(!ok);
    assert!(background::log_tail(&config, 5).iter().any(|l| l.contains("Could not start") && l.contains("user add")));
    assert_eq!(background::status(&config), Background::NotRunning);

    // Nor when it cannot take its lock: the log says so too.
    let lock = config.join("background.lock");
    std::fs::remove_file(&lock).unwrap();
    std::fs::create_dir(&lock).unwrap();
    let (ok, _, _) = run(&config, &serve, "");
    assert!(!ok);
    assert!(background::log_tail(&config, 1).iter().any(|l| l.contains("Could not start")), "{:?}", background::log_tail(&config, 5));
    std::fs::remove_dir(&lock).unwrap();

    run(&config, &["user", "add", "ana", "--password-stdin"], "correct horse\n");
    let (ok, out, _) = run(&config, &["stop"], "");
    assert!(ok && out.contains("No server"), "{out}");
    let mut child = Command::new(env!("CARGO_BIN_EXE_plancraft-server"))
        .args(serve)
        .arg("--config")
        .arg(&config)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let status = loop {
        match background::status(&config) {
            Background::Sharing(s) => break s,
            _ if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(50)),
            other => panic!("not sharing: {other:?}"),
        }
    };
    assert_eq!(status.folder, plancraft_core::io::canonical(&folder).unwrap());
    assert!(!status.announced);
    let mut c = plancraft_server::client::Client::connect(status.addr, &plancraft_server::wire::Trust::FirstUse, "test").unwrap();
    assert_eq!(c.login("ana", "correct horse").unwrap(), "ana");

    // A second one (a login while the first still runs) leaves it be, and is no failure.
    let (ok, out, _) = run(&config, &serve, "");
    assert!(ok && out.contains("already sharing"), "{out}");
    assert!(matches!(background::status(&config), Background::Sharing(_)));

    let (ok, out, err) = run(&config, &["stop"], "");
    assert!(ok && out.contains("Stopped"), "{out}{err}");
    // Ended cleanly: a service manager does not start it again.
    assert!(child.wait().unwrap().success());
    assert_eq!(background::status(&config), Background::NotRunning);
    let log = background::log_tail(&config, 50);
    assert!(log.iter().any(|l| l.contains("asked to stop sharing")), "{log:?}");
    assert!(log.last().unwrap().ends_with("  Stopped"), "{log:?}");
}
