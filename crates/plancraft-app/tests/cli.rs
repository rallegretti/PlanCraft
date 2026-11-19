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

/// `plancraft serve` runs PlanCraft Server from the app's own binary.
#[test]
fn plancraft_serve_is_the_server() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_plancraft")).args(["serve", "--help"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("PlanCraft Server"));
}

/// File › Share Folder runs `plancraft serve FOLDER --watch-stdin` from the app's own binary: it
/// says when it listens, and stops by itself once PlanCraft lets go of it (quits or crashes).
#[test]
fn share_folder_runs_the_server_from_plancraft_itself() {
    use plancraft_server::sharing::{Program, Sharing, State};
    use std::time::{Duration, Instant};
    let dir = std::env::temp_dir().join(format!("plancraft-share-cli-{}", std::process::id()));
    let (folder, config) = (dir.join("Plans"), dir.join("server"));
    std::fs::create_dir_all(&folder).unwrap();
    let mut users = plancraft_server::users::Users::load(&plancraft_server::server::users_path(&config)).unwrap();
    users.add("ana", "correct horse", plancraft_server::protocol::Role::Editor).unwrap();
    users.save().unwrap();
    // On this computer only (no firewall prompts on the build machines).
    let program = Program {
        program: env!("CARGO_BIN_EXE_plancraft").into(),
        args: vec!["serve".into(), "--bind".into(), "127.0.0.1".into()],
        prepare: None,
    };
    let mut s = Sharing::start(&program, &folder, &config, 0, false, std::sync::Arc::new(|_| {})).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let ready = loop {
        match s.state() {
            State::Sharing(r) => break r,
            State::Starting => {}
            other => panic!("{other:?}: {:?}", s.log()),
        }
        assert!(Instant::now() < deadline, "{:?}", s.log());
        std::thread::sleep(Duration::from_millis(50));
    };
    let addr = std::net::SocketAddr::new(std::net::Ipv4Addr::LOCALHOST.into(), ready.addr.port());
    let fingerprint = plancraft_server::wire::Fingerprint(ready.fingerprint.clone());
    let mut c = plancraft_server::client::Client::connect(addr, &plancraft_server::wire::Trust::Pinned(fingerprint), "test").unwrap();
    c.login("ana", "correct horse").unwrap();
    drop(c);
    drop(s);
    let deadline = Instant::now() + Duration::from_secs(30);
    while std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
        assert!(Instant::now() < deadline, "the server is still running");
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
