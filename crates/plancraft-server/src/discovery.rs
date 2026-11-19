//! Finding servers on the office network: a running server announces itself over mDNS (DNS
//! service discovery, `_plancraft._tcp`), and *Open from Server* lists the servers it hears of.
//! Pure Rust (`mdns-sd`).
//!
//! Neither runs unless someone shares a folder or opens the *Open from Server* window: nothing
//! announces itself or listens for announcements otherwise.
//!
//! An announcement proves nothing about who sent it (anyone on the network can send one). It only
//! fills in an address: the server's key is still checked against its fingerprint on the first
//! visit, and against the pinned one after that, whatever the announcement said.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::protocol::DEFAULT_PORT;

/// The service type PlanCraft servers announce.
pub const SERVICE: &str = "_plancraft._tcp.local.";
/// The standard mDNS port.
pub const MDNS_PORT: u16 = 5353;
/// Servers listed at most (announcements come from anyone on the network).
pub const MAX_FOUND: usize = 64;
/// Longest server name shown.
const MAX_NAME_CHARS: usize = 80;
/// A DNS label holds at most 63 bytes.
const MAX_LABEL_BYTES: usize = 63;
/// How long stopping waits for the goodbye message to go out.
const GOODBYE_WAIT: Duration = Duration::from_secs(1);

/// A server's announcement on the network, until dropped.
pub struct Announcer {
    daemon: ServiceDaemon,
    fullname: String,
}

impl std::fmt::Debug for Announcer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Announcer").field("fullname", &self.fullname).finish_non_exhaustive()
    }
}

/// `text` cut to at most `max` bytes, on a character boundary.
fn cut_bytes(text: &str, max: usize) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if out.len().saturating_add(c.len_utf8()) > max {
            break;
        }
        out.push(c);
    }
    out
}

/// The host name the server's announcement points to (`studio-pc-plancraft.local.`): the
/// computer's name in letters, digits and hyphens, with `-plancraft` added. Not the computer's
/// own `.local` name, which the system answers for: two answers with different addresses for one
/// name could make the system think another computer took its name, and rename itself.
fn mdns_host(host: &str) -> String {
    const SUFFIX: &str = "-plancraft";
    let label: String = host
        .split('.')
        .next()
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let label = cut_bytes(label.trim_matches('-'), MAX_LABEL_BYTES.saturating_sub(SUFFIX.len()));
    let label = label.trim_end_matches('-');
    if label.is_empty() { "plancraft-server.local.".to_string() } else { format!("{label}{SUFFIX}.local.") }
}

/// The name announced for a server: what people see in the list (`Plans on studio-pc`), as one
/// DNS label with no control characters.
pub fn instance_name(folder: &str, host: &str) -> String {
    let clean = |s: &str| s.chars().filter(|c| !c.is_control()).collect::<String>().trim().to_string();
    let (folder, host) = (clean(folder), clean(host));
    let name = match (folder.is_empty(), host.is_empty()) {
        (false, false) => format!("{folder} on {host}"),
        (false, true) => folder,
        (true, false) => format!("PlanCraft Server on {host}"),
        (true, true) => "PlanCraft Server".to_string(),
    };
    cut_bytes(&name, MAX_LABEL_BYTES)
}

impl Announcer {
    /// Announces a server called `name` listening at `addr` on the standard mDNS port. A server
    /// listening on every address (`0.0.0.0`) announces each of the computer's addresses, kept up
    /// to date as they change.
    pub fn start(name: &str, host: &str, addr: std::net::SocketAddr) -> Result<Announcer, String> {
        Self::start_on(name, host, addr, MDNS_PORT)
    }

    /// [`Announcer::start`] on another mDNS port (tests, so they never reach the real network).
    pub fn start_on(name: &str, host: &str, addr: std::net::SocketAddr, mdns_port: u16) -> Result<Announcer, String> {
        let daemon = ServiceDaemon::new_with_port(mdns_port).map_err(|e| format!("cannot announce the server on the network: {e}"))?;
        skip_virtual_interfaces(&daemon);
        let ip = if addr.ip().is_unspecified() { String::new() } else { addr.ip().to_string() };
        let properties = [("v", crate::protocol::PROTOCOL.to_string())];
        let info = ServiceInfo::new(SERVICE, &instance_name(name, ""), &mdns_host(host), ip.as_str(), addr.port(), &properties[..])
            .map_err(|e| format!("cannot announce the server on the network: {e}"))?;
        let info = if addr.ip().is_unspecified() { info.enable_addr_auto() } else { info };
        let fullname = info.get_fullname().to_string();
        if let Err(e) = daemon.register(info) {
            let _ = daemon.shutdown();
            return Err(format!("cannot announce the server on the network: {e}"));
        }
        Ok(Announcer { daemon, fullname })
    }
}

impl Drop for Announcer {
    /// Says goodbye (so lists drop the server at once), then stops.
    fn drop(&mut self) {
        if let Ok(done) = self.daemon.unregister(&self.fullname) {
            let _ = done.recv_timeout(GOODBYE_WAIT);
        }
        if let Ok(done) = self.daemon.shutdown() {
            let _ = done.recv_timeout(GOODBYE_WAIT);
        }
    }
}

/// A server heard of on the network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// What it calls itself (`Plans on studio-pc`): from the network, so not to be trusted.
    pub name: String,
    /// The address to connect to, as people type it (`192.168.1.20`, `192.168.1.20:47400`).
    pub address: String,
    /// Every address it gave, best first (`address` is the first): one already trusted is
    /// preferred to the best, as anyone can add addresses to another server's announcement.
    pub addresses: Vec<String>,
}

/// The address to connect to among those a server announced: an IPv4 address on the network
/// first, then a global IPv6 one, then the loopback address (the server runs on this computer).
/// Link-local IPv6 addresses need an interface to be named, so they are passed over.
pub fn pick_address(addresses: &[IpAddr], port: u16) -> Option<String> {
    ranked_addresses(addresses, port).into_iter().next()
}

/// The addresses to connect to among those a server announced, best first (see
/// [`pick_address`]), at most eight.
fn ranked_addresses(addresses: &[IpAddr], port: u16) -> Vec<String> {
    let rank = |ip: &IpAddr| match ip {
        IpAddr::V4(v4) if v4.is_unspecified() || v4.is_multicast() || v4.is_broadcast() => None,
        IpAddr::V4(v4) if v4.is_loopback() => Some(3),
        IpAddr::V4(v4) if v4.is_link_local() => Some(2),
        IpAddr::V4(_) => Some(0),
        IpAddr::V6(v6) if v6.is_unspecified() || v6.is_multicast() => None,
        IpAddr::V6(v6) if v6.is_loopback() => Some(4),
        // fe80::/10: link-local.
        IpAddr::V6(v6) if v6.segments().first().is_some_and(|s| s & 0xffc0 == 0xfe80) => None,
        IpAddr::V6(_) => Some(1),
    };
    if port == 0 {
        return Vec::new();
    }
    let mut ranked: Vec<(u8, IpAddr)> = addresses.iter().filter_map(|ip| rank(ip).map(|r| (r, *ip))).collect();
    ranked.sort_unstable();
    ranked.dedup();
    ranked.into_iter().take(8).map(|(_, ip)| typed(ip, port)).collect()
}

/// An address as people type it in *Open from Server*: the port only when it is not the usual one.
fn typed(ip: IpAddr, port: u16) -> String {
    match (ip, port == DEFAULT_PORT) {
        (IpAddr::V4(ip), true) => ip.to_string(),
        (IpAddr::V4(ip), false) => format!("{ip}:{port}"),
        (IpAddr::V6(ip), true) => format!("[{ip}]"),
        (IpAddr::V6(ip), false) => format!("[{ip}]:{port}"),
    }
}

/// Whether others on the network can reach this computer at `ip`: not loopback, link-local,
/// multicast or unspecified.
fn reachable(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => !(v4.is_loopback() || v4.is_link_local() || v4.is_unspecified() || v4.is_multicast() || v4.is_broadcast()),
        IpAddr::V6(v6) => {
            !(v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() || v6.segments().first().is_some_and(|s| s & 0xffc0 == 0xfe80))
        }
    }
}

/// Interfaces only this computer reaches: container bridges and virtual machines' networks
/// (Docker, Podman, libvirt, VirtualBox, VMware, WSL and Hyper-V's Default Switch). A Hyper-V
/// external switch (`vEthernet (External…)` on Windows) holds the computer's real network
/// address, so it is kept.
fn virtual_interface(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let unix = ["docker", "br-", "virbr", "vboxnet", "vmnet", "podman", "cni", "flannel", "lxcbr", "lxdbr"];
    (name.starts_with("veth") && !name.starts_with("vethernet"))
        || unix.iter().any(|p| name.starts_with(p))
        || name.starts_with("vethernet (wsl")
        || name.contains("(default switch)")
}

/// Keeps a daemon off this computer's virtual interfaces (it would announce, and hear, addresses
/// only this computer reaches).
fn skip_virtual_interfaces(daemon: &ServiceDaemon) {
    let names: std::collections::BTreeSet<String> =
        if_addrs::get_if_addrs().map(|l| l.into_iter().map(|i| i.name).filter(|n| virtual_interface(n)).collect()).unwrap_or_default();
    if !names.is_empty() {
        let _ = daemon.disable_interface(names.into_iter().map(mdns_sd::IfKind::Name).collect::<Vec<_>>());
    }
}

/// The addresses of this computer that others on the network can type to reach a server on
/// `port`: IPv4 first, then IPv6.
pub fn local_addresses(port: u16) -> Vec<String> {
    let mut ips: Vec<IpAddr> = if_addrs::get_if_addrs()
        .map(|l| l.into_iter().filter(|i| !virtual_interface(&i.name)).map(|i| i.ip()).collect())
        .unwrap_or_default();
    ips.retain(reachable);
    ips.sort_by_key(|ip| (ip.is_ipv6(), *ip));
    ips.dedup();
    ips.into_iter().take(8).map(|ip| typed(ip, port)).collect()
}

/// Characters that change how text around them looks without showing themselves (direction
/// overrides, zero-width characters): a name could look like another with them.
fn invisible(c: char) -> bool {
    matches!(c, '\u{00ad}' | '\u{061c}' | '\u{180e}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}' | '\u{fff9}'..='\u{fffb}')
}

/// Marks PlanCraft draws beside servers it already trusts: a name must not start with one.
fn trust_mark(c: char) -> bool {
    matches!(c, '✔' | '✓' | '☑' | '✅' | '🔒' | '🔐')
}

/// What an announcement says, made safe to show: its name cut short with no control or
/// invisible characters and no trust mark in front, and the addresses to connect to. `None` when
/// it gives nothing to connect to.
pub fn found_from(fullname: &str, port: u16, addresses: &[IpAddr]) -> Option<Found> {
    let addresses = ranked_addresses(addresses, port);
    let address = addresses.first()?.clone();
    let name: String =
        mdns_sd::instance_name_unescaped(fullname).chars().filter(|c| !c.is_control() && !invisible(*c)).take(MAX_NAME_CHARS).collect();
    let name = name.trim_start_matches(|c: char| trust_mark(c) || c.is_whitespace()).trim_end();
    let name = if name.is_empty() { address.clone() } else { name.to_string() };
    Some(Found { name, address, addresses })
}

/// Listens for servers' announcements until dropped.
pub struct Browser {
    daemon: ServiceDaemon,
    events: mdns_sd::Receiver<ServiceEvent>,
    found: BTreeMap<String, Found>,
}

impl Browser {
    /// Starts listening on the standard mDNS port.
    pub fn start() -> Result<Browser, String> {
        Self::start_on(MDNS_PORT)
    }

    /// [`Browser::start`] on another mDNS port (tests).
    pub fn start_on(mdns_port: u16) -> Result<Browser, String> {
        let daemon = ServiceDaemon::new_with_port(mdns_port).map_err(|e| format!("cannot look for servers on the network: {e}"))?;
        skip_virtual_interfaces(&daemon);
        let events = match daemon.browse(SERVICE) {
            Ok(r) => r,
            Err(e) => {
                let _ = daemon.shutdown();
                return Err(format!("cannot look for servers on the network: {e}"));
            }
        };
        Ok(Browser { daemon, events, found: BTreeMap::new() })
    }

    /// Takes in the announcements heard since the last call; returns whether the list changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        // Everything waiting (the daemon gives up on a listener that lets 1,000 pile up), up to a
        // bound, so a flood never holds up a frame for long.
        for _ in 0..4096 {
            let event = match self.events.try_recv() {
                Ok(e) => e,
                Err(mdns_sd::TryRecvError::Empty) => break,
                Err(mdns_sd::TryRecvError::Disconnected) => {
                    // Given up on (flooded), or its thread ended: listen afresh.
                    if let Ok(events) = self.daemon.browse(SERVICE) {
                        self.events = events;
                    }
                    break;
                }
            };
            match event {
                ServiceEvent::ServiceResolved(s) if s.ty_domain == SERVICE => {
                    let addresses: Vec<IpAddr> = s.addresses.iter().map(mdns_sd::ScopedIp::to_ip_addr).collect();
                    let Some(found) = found_from(&s.fullname, s.port, &addresses) else { continue };
                    if self.found.len() >= MAX_FOUND && !self.found.contains_key(&s.fullname) {
                        continue;
                    }
                    if self.found.get(&s.fullname) != Some(&found) {
                        self.found.insert(s.fullname.clone(), found);
                        changed = true;
                    }
                }
                ServiceEvent::ServiceRemoved(_, fullname) => changed |= self.found.remove(&fullname).is_some(),
                _ => {}
            }
        }
        changed
    }

    /// The servers heard of, by name.
    pub fn found(&self) -> Vec<Found> {
        let mut list: Vec<Found> = self.found.values().cloned().collect();
        list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.address.cmp(&b.address)));
        list
    }
}

impl Drop for Browser {
    /// Stops listening, waiting for the daemon on a thread of its own (closing the window must not
    /// wait for it).
    fn drop(&mut self) {
        fn stop(daemon: &ServiceDaemon) {
            let _ = daemon.stop_browse(SERVICE);
            if let Ok(done) = daemon.shutdown() {
                let _ = done.recv_timeout(GOODBYE_WAIT);
            }
        }
        let daemon = self.daemon.clone();
        if std::thread::Builder::new().name("plancraft-mdns-stop".into()).spawn(move || stop(&daemon)).is_err() {
            stop(&self.daemon);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn the_best_address_is_picked() {
        let lan = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20));
        let local6 = IpAddr::V6("fe80::1".parse::<Ipv6Addr>().unwrap());
        let global6 = IpAddr::V6("2001:db8::5".parse::<Ipv6Addr>().unwrap());
        let lo = IpAddr::V4(Ipv4Addr::LOCALHOST);
        assert_eq!(pick_address(&[local6, global6, lo, lan], DEFAULT_PORT).as_deref(), Some("192.168.1.20"));
        assert_eq!(pick_address(&[lan], 47400).as_deref(), Some("192.168.1.20:47400"));
        assert_eq!(pick_address(&[local6, global6], DEFAULT_PORT).as_deref(), Some("[2001:db8::5]"));
        assert_eq!(pick_address(&[global6], 1).as_deref(), Some("[2001:db8::5]:1"));
        assert_eq!(pick_address(&[local6, lo], DEFAULT_PORT).as_deref(), Some("127.0.0.1"));
        assert_eq!(pick_address(&[local6], DEFAULT_PORT), None);
        assert_eq!(pick_address(&[lan], 0), None);
        assert_eq!(pick_address(&[], DEFAULT_PORT), None);
        assert_eq!(pick_address(&[IpAddr::V4(Ipv4Addr::UNSPECIFIED), IpAddr::V4(Ipv4Addr::new(224, 0, 0, 251))], DEFAULT_PORT), None);
        // Every address picked is one the Open from Server window takes.
        for a in [pick_address(&[lan], 47400), pick_address(&[global6], DEFAULT_PORT), pick_address(&[global6], 9)] {
            assert!(crate::link::normalise_address(&a.unwrap()).is_ok());
        }
    }

    #[test]
    fn container_bridges_are_not_offered() {
        for name in ["docker0", "br-3f2a", "veth12", "virbr0", "vboxnet0", "vmnet8", "podman0"] {
            assert!(virtual_interface(name), "{name}");
        }
        for name in ["vEthernet (WSL (Hyper-V firewall))", "vEthernet (Default Switch)"] {
            assert!(virtual_interface(name), "{name}");
        }
        // A Hyper-V external switch holds the computer's real address.
        for name in ["eth0", "enp5s0", "wlan0", "en0", "Ethernet", "Wi-Fi", "tailscale0", "wg0", "vEthernet (External Switch)"] {
            assert!(!virtual_interface(name), "{name}");
        }
    }

    #[test]
    fn local_addresses_can_be_typed() {
        for a in local_addresses(47400) {
            assert!(crate::link::normalise_address(&a).is_ok(), "{a}");
            assert!(!a.starts_with("127.") && !a.starts_with("[::1]"), "{a}");
        }
    }

    #[test]
    fn announcements_are_made_safe_to_show() {
        let lan = [IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2))];
        let f = found_from(r"Plans on studio\.pc._plancraft._tcp.local.", DEFAULT_PORT, &lan).unwrap();
        assert_eq!(f, Found { name: "Plans on studio.pc".into(), address: "10.0.0.2".into(), addresses: vec!["10.0.0.2".into()] });
        // No fake trust mark, no hidden characters that make a name look like another.
        let f = found_from("✔ \u{202e}Plans\u{200b} on studio-pc._plancraft._tcp.local.", DEFAULT_PORT, &lan).unwrap();
        assert_eq!(f.name, "Plans on studio-pc");
        // Every address is kept, best first: one already trusted can be picked over the best.
        let many = [IpAddr::V4(Ipv4Addr::new(192, 168, 1, 9)), IpAddr::V4(Ipv4Addr::new(1, 0, 0, 1)), IpAddr::V4(Ipv4Addr::LOCALHOST)];
        let f = found_from("x._plancraft._tcp.local.", DEFAULT_PORT, &many).unwrap();
        assert_eq!(f.addresses, ["1.0.0.1", "192.168.1.9", "127.0.0.1"]);
        let f = found_from("Evil\u{1b}[2J\u{7}name._plancraft._tcp.local.", DEFAULT_PORT, &lan).unwrap();
        assert!(!f.name.chars().any(char::is_control), "{:?}", f.name);
        let long = format!("{}._plancraft._tcp.local.", "x".repeat(10_000));
        assert_eq!(found_from(&long, DEFAULT_PORT, &lan).unwrap().name.chars().count(), MAX_NAME_CHARS);
        assert_eq!(found_from("._plancraft._tcp.local.", DEFAULT_PORT, &lan).unwrap().name, "10.0.0.2");
        assert_eq!(found_from("x._plancraft._tcp.local.", DEFAULT_PORT, &[]), None);
    }

    #[test]
    fn names_fit_in_one_label() {
        assert_eq!(instance_name("Plans", "studio-pc"), "Plans on studio-pc");
        assert_eq!(instance_name("", "studio-pc"), "PlanCraft Server on studio-pc");
        assert_eq!(instance_name("\u{7}", ""), "PlanCraft Server");
        let n = instance_name(&"é".repeat(100), "pc");
        assert!(n.len() <= MAX_LABEL_BYTES && n.chars().all(|c| c == 'é'));
        assert_eq!(mdns_host("Studio PC.example.com"), "studio-pc-plancraft.local.");
        assert!(mdns_host(&"x".repeat(200)).trim_end_matches(".local.").len() <= MAX_LABEL_BYTES);
        assert_eq!(mdns_host("ünï"), "n-plancraft.local.");
        assert_eq!(mdns_host("ü"), "plancraft-server.local.");
    }
}
