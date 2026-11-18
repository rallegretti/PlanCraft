#![allow(dead_code)]
//! A server on a free local port, sharing a temporary folder with two plans, and its users.

use std::net::SocketAddr;
use std::path::PathBuf;

use plancraft_core::dates::ymd;
use plancraft_core::sample::demo_project;
use plancraft_server::client::Client;
use plancraft_server::protocol::Role;
use plancraft_server::server::{self, Options, Running};
use plancraft_server::users::Users;
use plancraft_server::wire::Trust;

pub const PASSWORD: &str = "correct horse";

pub struct Setup {
    pub dir: tempfile::TempDir,
    pub folder: PathBuf,
    pub config: PathBuf,
}

/// A folder with "Plan.plancraft" and "Secret.plancraft"; users ana and ben (editors), vic
/// (viewer) and eve (editor, but not of Secret.plancraft).
pub fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("shared");
    let config = dir.path().join("config");
    std::fs::create_dir_all(&folder).unwrap();
    plancraft_core::io::save(&demo_project(ymd(2026, 1, 5).unwrap()), folder.join("Plan.plancraft")).unwrap();
    plancraft_core::io::save(&demo_project(ymd(2026, 1, 5).unwrap()), folder.join("Secret.plancraft")).unwrap();
    std::fs::write(folder.join("notes.txt"), "not a plan").unwrap();
    let mut users = Users::load(&server::users_path(&config)).unwrap();
    users.add("ana", PASSWORD, Role::Editor).unwrap();
    users.add("ben", PASSWORD, Role::Editor).unwrap();
    users.add("vic", PASSWORD, Role::Viewer).unwrap();
    users.add("eve", PASSWORD, Role::Editor).unwrap();
    users.set_role("eve", Role::None, Some("Secret.plancraft")).unwrap();
    users.save().unwrap();
    Setup { dir, folder, config }
}

impl Setup {
    /// The tests all connect from 127.0.0.1, many at once: the per-address limit on connections
    /// not signed in yet is raised (its own test uses the default).
    pub fn options(&self) -> Options {
        let mut o =
            Options::new(self.folder.clone(), self.config.clone(), "127.0.0.1:0".parse::<SocketAddr>().unwrap(), plancraft_server::quiet());
        o.not_signed_in_per_address = 64;
        o
    }

    pub fn start(&self) -> Running {
        server::start(self.options()).unwrap()
    }
}

pub fn connect(server: &Running) -> Client {
    let c = Client::connect(server.addr(), &Trust::Pinned(server.fingerprint().clone()), "test").unwrap();
    c.set_read_timeout(Some(std::time::Duration::from_secs(20))).unwrap();
    c
}

pub fn signed_in(server: &Running, user: &str) -> Client {
    let mut c = connect(server);
    c.login(user, PASSWORD).unwrap();
    c.set_read_timeout(Some(std::time::Duration::from_secs(20))).unwrap();
    c
}
