#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! *File › Share Folder…*: a server run as a separate program (`sharing::Sharing`), and servers
//! finding each other on the network (`discovery`).

mod common;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use plancraft_server::client::Client;
use plancraft_server::discovery::{Announcer, Browser};
use plancraft_server::protocol::ClientId;
use plancraft_server::sharing::{Program, Sharing, State};
use plancraft_server::wire::{Fingerprint, Trust};

/// The server, listening on this computer only (no firewall prompts on the build machines).
fn program() -> Program {
    Program { program: env!("CARGO_BIN_EXE_plancraft-server").into(), args: vec!["--bind".into(), "127.0.0.1".into()], prepare: None }
}

fn waker() -> plancraft_server::sharing::Waker {
    Arc::new(|_| {})
}

/// Waits (up to 30 s) until `done` says yes about the sharing's state.
fn wait_for(s: &mut Sharing, done: impl Fn(&State) -> bool) -> State {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let state = s.state();
        if done(&state) {
            return state;
        }
        assert!(Instant::now() < deadline, "still {state:?}; log: {:?}", s.log());
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_shared_folder_is_served_and_stopped_cleanly() {
    let setup = common::setup();
    let mut s = Sharing::start(&program(), &setup.folder, &setup.config, 0, false, waker()).unwrap();
    let State::Sharing(ready) = wait_for(&mut s, |st| !matches!(st, State::Starting)) else { panic!("log: {:?}", s.log()) };
    assert_eq!(s.folder(), setup.folder);
    // The window shows the fingerprint people check: it is the server's.
    let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), ready.addr.port());
    let mut c = Client::connect(addr, &Trust::Pinned(Fingerprint(ready.fingerprint.clone())), "test").unwrap();
    c.login("ana", common::PASSWORD).unwrap();
    c.open("Plan.plancraft", ClientId::default(), None, None).unwrap();
    let lock = setup.folder.join(".Plan.plancraft.lock");
    assert!(lock.exists(), "the server holds the plan's lock while sharing it");
    assert!(s.log().iter().any(|l| l.contains("Listening on")), "{:?}", s.log());

    s.stop();
    assert_eq!(wait_for(&mut s, |st| matches!(st, State::Stopped(_))), State::Stopped(None), "log: {:?}", s.log());
    assert!(!lock.exists(), "stopping gives the lock back");
    assert!(s.log().iter().any(|l| l.contains("Stopping")), "{:?}", s.log());
}

#[test]
fn a_server_that_cannot_start_says_why() {
    let setup = common::setup();
    std::fs::remove_file(setup.config.join("users.json")).unwrap();
    let mut s = Sharing::start(&program(), &setup.folder, &setup.config, 0, false, waker()).unwrap();
    let State::Stopped(Some(why)) = wait_for(&mut s, |st| matches!(st, State::Stopped(_))) else { panic!() };
    assert!(why.contains("user add") && !why.starts_with("plancraft-server:"), "{why}");

    let mut s = Sharing::start(&program(), &setup.dir.path().join("missing"), &setup.config, 0, false, waker()).unwrap();
    let State::Stopped(Some(why)) = wait_for(&mut s, |st| matches!(st, State::Stopped(_))) else { panic!() };
    assert!(why.contains("cannot open the folder"), "{why}");

    // A folder whose name starts with "-" is still a folder, not an option.
    let mut s = Sharing::start(&program(), &setup.dir.path().join("-missing"), &setup.config, 0, false, waker()).unwrap();
    let State::Stopped(Some(why)) = wait_for(&mut s, |st| matches!(st, State::Stopped(_))) else { panic!() };
    assert!(why.contains("cannot open the folder"), "{why}");

    let missing = Program { program: setup.dir.path().join("no-such-program"), args: Vec::new(), prepare: None };
    assert!(Sharing::start(&missing, &setup.folder, &setup.config, 0, false, waker()).unwrap_err().contains("cannot start the server"));
}

#[test]
fn dropping_the_sharing_stops_the_server() {
    // As when PlanCraft quits (or crashes): the server's input closes and it stops by itself,
    // saving its plans and giving back their locks.
    let setup = common::setup();
    let mut s = Sharing::start(&program(), &setup.folder, &setup.config, 0, false, waker()).unwrap();
    let State::Sharing(ready) = wait_for(&mut s, |st| !matches!(st, State::Starting)) else { panic!("log: {:?}", s.log()) };
    let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), ready.addr.port());
    let mut c = Client::connect(addr, &Trust::Pinned(Fingerprint(ready.fingerprint.clone())), "test").unwrap();
    c.login("ana", common::PASSWORD).unwrap();
    c.open("Plan.plancraft", ClientId::default(), None, None).unwrap();
    let lock = setup.folder.join(".Plan.plancraft.lock");
    assert!(lock.exists());
    drop(s);
    let deadline = Instant::now() + Duration::from_secs(30);
    while lock.exists() || std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
        assert!(Instant::now() < deadline, "the server is still running");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_server_goes_on_saving_after_plancraft_is_gone() {
    // As when PlanCraft quits: nobody reads the server's output any more. Writing its log must
    // not stop it (a panic on a closed pipe), and it saves its plans and gives back their locks.
    use std::io::BufRead;
    let setup = common::setup();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_plancraft-server"))
        .args(["--bind", "127.0.0.1", "--port", "0", "--watch-stdin", "--no-announce", "--config"])
        .arg(&setup.config)
        .arg("--")
        .arg(&setup.folder)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut out = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    out.read_line(&mut line).unwrap();
    let ready = plancraft_server::cli::Ready::parse(line.trim_end()).unwrap_or_else(|| panic!("{line}"));
    drop(out);
    drop(child.stderr.take());
    let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), ready.addr.port());
    let mut c = Client::connect(addr, &Trust::Pinned(Fingerprint(ready.fingerprint)), "test").unwrap();
    c.login("ana", common::PASSWORD).unwrap();
    c.open("Plan.plancraft", ClientId::default(), None, None).unwrap();
    let lock = setup.folder.join(".Plan.plancraft.lock");
    assert!(lock.exists());
    drop(child.stdin.take());
    let status = child.wait().unwrap();
    assert!(status.success(), "{status}");
    assert!(!lock.exists(), "the lock is given back");
}

/// A private mDNS port for a test, different between runs (two build machines on one network
/// run the tests at the same time).
fn private_mdns_port() -> u16 {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    45_353 + ((std::process::id() ^ nanos) % 2000) as u16
}

#[test]
fn a_shared_folder_is_announced_while_it_is_shared() {
    let port = private_mdns_port();
    let mut browser = match Browser::start_on(port) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("skipped: no multicast here ({e})");
            return;
        }
    };
    let setup = common::setup();
    let mut options = setup.options();
    options.bind = "0.0.0.0:0".parse().unwrap();
    options.announce = true;
    options.mdns_port = port;
    let server = plancraft_server::server::start(options).unwrap();
    let wanted = format!(":{}", server.addr().port());
    let deadline = Instant::now() + Duration::from_secs(20);
    let found = loop {
        browser.poll();
        if let Some(f) = browser.found().into_iter().find(|f| f.address.ends_with(&wanted) && f.name.starts_with("shared on ")) {
            break f;
        }
        assert!(Instant::now() < deadline, "the server's announcement was not heard: {:?}", browser.found());
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(plancraft_server::link::normalise_address(&found.address).is_ok(), "{found:?}");
    server.stop();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        browser.poll();
        if !browser.found().iter().any(|f| f.name == found.name && f.address == found.address) {
            break;
        }
        assert!(Instant::now() < deadline, "the stopped server is still listed");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn servers_are_found_on_the_network_and_go_away_when_stopped() {
    // A private mDNS port: nothing reaches anyone listening on the usual one (the messages still
    // go out on the network).
    let port = private_mdns_port();
    let mut browser = match Browser::start_on(port) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("skipped: no multicast here ({e})");
            return;
        }
    };
    let server: SocketAddr = "0.0.0.0:47411".parse().unwrap();
    let name = format!("Plans on test-pc-{}", std::process::id());
    let announcer = Announcer::start_on(&name, "test-pc", server, port).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let found = loop {
        browser.poll();
        if let Some(f) = browser.found().into_iter().find(|f| f.name == name) {
            break f;
        }
        assert!(Instant::now() < deadline, "the announcement was not heard");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(found.address.ends_with(":47411"), "{found:?}");
    assert!(plancraft_server::link::normalise_address(&found.address).is_ok());
    drop(announcer);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        browser.poll();
        if !browser.found().iter().any(|f| f.name == name) {
            break;
        }
        assert!(Instant::now() < deadline, "the stopped server is still listed");
        std::thread::sleep(Duration::from_millis(50));
    }
}

proptest::proptest! {
    /// Announcements come from anyone on the network: whatever they say, reading one never
    /// panics, and what is shown is short, has no control characters, and is an address the
    /// Open from Server window takes.
    #[test]
    fn any_announcement_reads_safely(
        name in proptest::prelude::any::<String>(),
        port in proptest::prelude::any::<u16>(),
        v4 in proptest::collection::vec(proptest::prelude::any::<[u8; 4]>(), 0..6),
        v6 in proptest::collection::vec(proptest::prelude::any::<[u16; 8]>(), 0..6),
    ) {
        let mut addresses: Vec<std::net::IpAddr> = v4.into_iter().map(|o| std::net::Ipv4Addr::from(o).into()).collect();
        addresses.extend(v6.into_iter().map(|s| std::net::IpAddr::from(std::net::Ipv6Addr::from(s))));
        let fullname = format!("{name}.{}", plancraft_server::discovery::SERVICE);
        if let Some(f) = plancraft_server::discovery::found_from(&fullname, port, &addresses) {
            proptest::prop_assert!(f.name.chars().count() <= 80 && !f.name.is_empty());
            proptest::prop_assert!(!f.name.chars().any(char::is_control));
            proptest::prop_assert!(plancraft_server::link::normalise_address(&f.address).is_ok(), "{}", f.address);
        }
    }
}
