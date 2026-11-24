#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! Keep sharing at login, with the system's own login items (a systemd user unit, a LaunchAgent,
//! the Run key). Ignored by default, as it installs one for a moment: run it on a machine where
//! that is fine with `cargo test -p plancraft-server --test login -- --ignored`. It refuses to
//! touch a login item that is already there.

use std::time::{Duration, Instant};

use plancraft_server::background::{self, Background};
use plancraft_server::login;
use plancraft_server::sharing::Program;

#[test]
#[ignore = "installs a login item for a moment: run with --ignored where that is fine"]
fn the_system_starts_the_server_which_stops_when_asked() {
    assert!(!login::enabled(), "a login item is already installed here: not touching it");
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config");
    let folder = dir.path().join("Shared plans");
    std::fs::create_dir(&folder).unwrap();
    let password = plancraft_server::users::NewPassword::new("correct horse").unwrap();
    plancraft_server::users::Users::change(&plancraft_server::server::users_path(&config), |u| {
        u.add_with("ana", &password, plancraft_server::protocol::Role::Editor)
    })
    .unwrap();
    // Only on this computer while the test runs.
    let program =
        Program { program: env!("CARGO_BIN_EXE_plancraft-server").into(), args: vec!["--bind".into(), "127.0.0.1".into()], prepare: None };
    let entry = login::Entry::new(&program, &folder, &config, 0, false).unwrap();

    login::enable(&entry).unwrap();
    let cleanup = scopeguard(&config);
    assert!(login::enabled());
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        match background::status(&config) {
            Background::Sharing(s) => break s,
            _ if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            other => panic!("the system did not start it: {other:?}\n{}", background::log_tail(&config, 20).join("\n")),
        }
    };
    assert_eq!(status.folder, plancraft_core::io::canonical(&folder).unwrap());
    let addr = std::net::SocketAddr::new(std::net::Ipv4Addr::LOCALHOST.into(), status.addr.port());
    let mut c = plancraft_server::client::Client::connect(addr, &plancraft_server::wire::Trust::FirstUse, "test").unwrap();
    assert_eq!(c.login("ana", "correct horse").unwrap(), "ana");
    drop(c);

    // What Stop Sharing does: asks the server to stop and waits, then takes the login item away.
    background::request_stop(&config).unwrap();
    assert!(background::wait_stopped(&config, Duration::from_secs(60)), "{}", background::log_tail(&config, 20).join("\n"));
    // Stopped cleanly: the system does not start it again.
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(background::status(&config), Background::NotRunning);
    login::disable(None).unwrap();
    assert!(!login::enabled());
    // Stopped by the request (not by a signal), every plan saved.
    let log = background::log_tail(&config, 50);
    assert!(log.iter().any(|l| l.contains("asked to stop sharing")), "{log:?}");
    assert!(log.last().is_some_and(|l| l.ends_with("  Stopped")), "{log:?}");
    drop(cleanup);
}

/// Takes the login item away and stops the server, also when the test fails part-way.
struct Cleanup(std::path::PathBuf);

fn scopeguard(config: &std::path::Path) -> Cleanup {
    Cleanup(config.to_path_buf())
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = login::disable(None);
        if background::status(&self.0) != Background::NotRunning {
            let _ = background::request_stop(&self.0);
            let _ = background::wait_stopped(&self.0, Duration::from_secs(60));
        }
    }
}
