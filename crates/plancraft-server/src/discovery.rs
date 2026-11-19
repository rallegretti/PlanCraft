//! Finding servers on the office network: a running server announces itself over mDNS (DNS
//! service discovery, `_plancraft._tcp`), and *Open from Server* lists the servers it hears of.
//! The protocol is PlanCraft's own (`crate::mdns`): it keeps only what it needs, bounded.
//!
//! Neither runs unless someone shares a folder or opens the *Open from Server* window: nothing
//! announces itself or listens for announcements otherwise.
//!
//! An announcement proves nothing about who sent it (anyone on the network can send one). It only
//! fills in an address: the server's key is still checked against its fingerprint on the first
//! visit, and against the pinned one after that, whatever the announcement said.

use std::collections::BTreeMap;
use std::net::IpAddr;

use crate::mdns::{self, Name, Net};
use crate::protocol::DEFAULT_PORT;

/// The service type PlanCraft servers announce.
pub const SERVICE: &str = "_plancraft._tcp.local.";
/// The standard mDNS port.
pub const MDNS_PORT: u16 = 5353;
/// Servers listed at most (announcements come from anyone on the network), besides those already
/// trusted.
pub const MAX_FOUND: usize = 64;
/// Servers listed at most per host name, and per address: one computer announcing many names
/// can't fill the list.
pub const MAX_PER_HOST: usize = 4;
/// Longest server name shown.
const MAX_NAME_CHARS: usize = 80;
/// A DNS label holds at most 63 bytes.
const MAX_LABEL_BYTES: usize = 63;
/// A server's announcement on the network, until dropped (then it says goodbye).
pub struct Announcer {
    _responder: mdns::Responder,
    fullname: String,
}

impl std::fmt::Debug for Announcer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Announcer").field("fullname", &self.fullname).finish_non_exhaustive()
    }
}

/// The service type as a name.
fn service_name() -> Name {
    Name::dotted(SERVICE)
}

/// A server's full name as it is keyed (`Plans\.2 on pc._plancraft._tcp.local.`).
fn fullname_of(label: &[u8]) -> String {
    format!("{}.{SERVICE}", mdns::escape_label(label))
}

/// The instance name in a full name, unescaped (`Plans.2 on pc`).
fn instance_unescaped(fullname: &str) -> String {
    let label = fullname
        .len()
        .checked_sub(SERVICE.len())
        .and_then(|cut| fullname.get(cut..).filter(|t| t.eq_ignore_ascii_case(SERVICE)).and(fullname.get(..cut)))
        .map(|l| l.strip_suffix('.').unwrap_or(l))
        .unwrap_or(fullname);
    let mut out = String::new();
    let mut chars = label.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.extend(chars.next()),
            c => out.push(c),
        }
    }
    out
}

/// The networks mDNS uses: this computer's IPv4 networks, but not loopback, container and
/// virtual-machine bridges, or VPN and overlay networks.
fn mdns_nets() -> Vec<Net> {
    let mut nets: Vec<Net> = if_addrs::get_if_addrs()
        .map(|l| {
            l.into_iter()
                .filter(|i| !virtual_interface(&i.name) && !overlay_interface(&i.name))
                .filter_map(|i| match i.addr {
                    if_addrs::IfAddr::V4(v4) if !v4.ip.is_loopback() && !v4.ip.is_link_local() => Some(Net { ip: v4.ip, mask: v4.netmask }),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    nets.sort_by_key(|n| u32::from(n.ip));
    nets.dedup();
    nets.truncate(16);
    nets
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

    /// [`Announcer::start`] on another mDNS port (tests: nothing reaches anyone listening on the
    /// usual one).
    pub fn start_on(name: &str, host: &str, addr: std::net::SocketAddr, mdns_port: u16) -> Result<Announcer, String> {
        let only = match addr.ip() {
            IpAddr::V4(v4) if !v4.is_unspecified() => Some(v4),
            IpAddr::V4(_) => None,
            IpAddr::V6(v6) if v6.is_unspecified() => None,
            IpAddr::V6(_) => return Err("servers listening on one IPv6 address are not announced (people type the address)".into()),
        };
        let label = instance_name(name, "").into_bytes();
        let fullname = fullname_of(&label);
        let service = mdns::Service {
            service: service_name(),
            instance: label,
            host: Name::dotted(&mdns_host(host)),
            port: addr.port(),
            txt: vec![format!("v={}", crate::protocol::PROTOCOL)],
            only,
        };
        let responder =
            mdns::Responder::start(service, mdns_port, mdns_nets).map_err(|e| format!("cannot announce the server on the network: {e}"))?;
        Ok(Announcer { _responder: responder, fullname })
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
    usable_addresses(addresses, port).into_iter().take(8).map(|ip| typed(ip, port)).collect()
}

/// How good an address is to connect to (lower is better); `None`: not one to connect to.
fn rank(ip: &IpAddr) -> Option<u8> {
    match ip {
        IpAddr::V4(v4) if v4.is_unspecified() || v4.is_multicast() || v4.is_broadcast() => None,
        // Loopback means "this computer" to whoever connects: never an address the network gives.
        IpAddr::V4(v4) if v4.is_loopback() => None,
        IpAddr::V4(v4) if v4.is_link_local() => Some(2),
        IpAddr::V4(_) => Some(0),
        IpAddr::V6(v6) if v6.is_unspecified() || v6.is_multicast() => None,
        IpAddr::V6(v6) if v6.is_loopback() => None,
        // fe80::/10: link-local.
        IpAddr::V6(v6) if v6.segments().first().is_some_and(|s| s & 0xffc0 == 0xfe80) => None,
        IpAddr::V6(_) => Some(1),
    }
}

/// Every address to connect to among `addresses`, best first, each once (none without a port).
fn usable_addresses(addresses: &[IpAddr], port: u16) -> Vec<IpAddr> {
    if port == 0 {
        return Vec::new();
    }
    let mut ranked: Vec<(u8, IpAddr)> = addresses.iter().filter_map(|ip| rank(ip).map(|r| (r, *ip))).collect();
    ranked.sort_unstable();
    ranked.dedup();
    ranked.into_iter().map(|(_, ip)| ip).collect()
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
    // macOS: bridge100 and up are virtual machines' (bridge0 is a Thunderbolt bridge).
    let mac_vm_bridge = name.strip_prefix("bridge").and_then(|n| n.parse::<u32>().ok()).is_some_and(|n| n >= 100);
    (name.starts_with("veth") && !name.starts_with("vethernet"))
        || unix.iter().any(|p| name.starts_with(p))
        || mac_vm_bridge
        || name.starts_with("vnic")
        || name.starts_with("vethernet (wsl")
        || name.contains("(default switch)")
        || name.contains("virtualbox host-only")
        || name.starts_with("vmware network adapter")
}

/// VPN and overlay networks (WireGuard, Tailscale, ZeroTier, tunnels): reachable, but announcing
/// on them tells the folder's and computer's names to everyone on that network.
fn overlay_interface(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["tun", "utun", "tap", "wg", "tailscale", "zt", "ipsec", "ppp"].iter().any(|p| name.starts_with(p))
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
    matches!(c,
        // Format characters: soft hyphen, direction marks and overrides, zero-width ones.
        '\u{00ad}' | '\u{061c}' | '\u{180e}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}'
        | '\u{feff}' | '\u{fff9}'..='\u{fffb}'
        // Combining marks (drawn over the character before), variation selectors, tags.
        | '\u{0300}'..='\u{036f}' | '\u{1ab0}'..='\u{1aff}' | '\u{1dc0}'..='\u{1dff}' | '\u{20d0}'..='\u{20ff}'
        | '\u{fe00}'..='\u{fe0f}' | '\u{fe20}'..='\u{fe2f}' | '\u{e0000}'..='\u{e007f}' | '\u{e0100}'..='\u{e01ef}'
        // Characters drawn blank: Hangul fillers, the blank Braille pattern, the grapheme joiner.
        | '\u{115f}' | '\u{1160}' | '\u{3164}' | '\u{ffa0}' | '\u{2800}')
}

/// Marks like the one PlanCraft draws beside servers it already trusts: a name holds none.
fn trust_mark(c: char) -> bool {
    matches!(c, '✔' | '✓' | '☑' | '✅' | '🗸' | '√' | '🔒' | '🔐' | '🛡')
}

/// What an announcement says, made safe to show: its name cut short with no control or
/// invisible characters and no trust marks, and the addresses to connect to. `None` when
/// it gives nothing to connect to.
pub fn found_from(fullname: &str, port: u16, addresses: &[IpAddr]) -> Option<Found> {
    let addresses = ranked_addresses(addresses, port);
    let address = addresses.first()?.clone();
    let name: String = instance_unescaped(fullname)
        .chars()
        .filter(|c| !c.is_control() && !invisible(*c) && !trust_mark(*c))
        .take(MAX_NAME_CHARS)
        .collect();
    let name = name.trim();
    let name = if name.is_empty() { address.clone() } else { name.to_string() };
    Some(Found { name, address, addresses })
}

/// One server heard of, and what it is counted by.
#[derive(Clone, Debug)]
struct Heard {
    found: Found,
    /// The host name its announcement points to, lower case.
    host: String,
    /// Every address it gave to connect to (counted without the port: one computer can announce
    /// on any number of ports), at most [`MAX_ADDRESSES_COUNTED`].
    ips: Vec<IpAddr>,
    /// Its port.
    port: u16,
    /// The address it gave whose key is trusted already (normalised), if any.
    trusted_at: Option<String>,
}

/// Addresses of one announcement looked at, at most.
const MAX_ADDRESSES_COUNTED: usize = 32;

/// The servers heard of, kept so no one computer can crowd out the others: at most
/// [`MAX_PER_HOST`] per host name and per address (whatever the port), [`MAX_FOUND`] in all,
/// first come first kept. A server at an address already trusted (its key was confirmed before)
/// always gets in and comes first, once per trusted address: anyone can add a trusted address to
/// an announcement, so a second one claiming it is not taken in (the window shows a trusted
/// server by the name it had when its key was confirmed, and connects to that address anyway).
#[derive(Default)]
pub struct FoundList {
    heard: BTreeMap<String, Heard>,
    /// Addresses whose key is trusted already, normalised.
    trusted: std::collections::HashSet<String>,
}

fn normalised(address: &str) -> String {
    crate::link::normalise_address(address).unwrap_or_else(|_| address.to_lowercase())
}

impl FoundList {
    /// The trusted address among `ips` on `port`, normalised, if any.
    fn trusted_among(&self, ips: &[IpAddr], port: u16) -> Option<String> {
        ips.iter().map(|ip| normalised(&typed(*ip, port))).find(|a| self.trusted.contains(a))
    }

    /// The addresses whose key is trusted already (as typed in *Open from Server*). Servers
    /// already listed are not looked at again (they are, as they announce themselves again).
    pub fn set_trusted<'a>(&mut self, addresses: impl IntoIterator<Item = &'a str>) {
        self.trusted = addresses.into_iter().map(normalised).collect();
        let mut claimed = std::collections::HashSet::new();
        let names: Vec<String> = self.heard.keys().cloned().collect();
        for name in names {
            let Some(h) = self.heard.get(&name) else { continue };
            let at = self.trusted_among(&h.ips, h.port);
            if let Some(h) = self.heard.get_mut(&name) {
                // One server per trusted address (the first, by name).
                h.trusted_at = at.filter(|a| claimed.insert(a.clone()));
            }
        }
    }

    /// Takes in a server's announcement (`fullname` as announced, the host name it points to,
    /// its port and addresses). Returns whether the list changed.
    pub fn heard(&mut self, fullname: &str, host: &str, port: u16, addresses: &[IpAddr]) -> bool {
        let Some(mut found) = found_from(fullname, port, addresses) else { return self.heard.remove(fullname).is_some() };
        let host = host.trim_end_matches('.').to_lowercase();
        let ips: Vec<IpAddr> = usable_addresses(addresses, port).into_iter().take(MAX_ADDRESSES_COUNTED).collect();
        // Among all its addresses, not only the best few (more addresses can push it out).
        let trusted_at = self.trusted_among(&ips, port);
        if let Some(at) = &trusted_at
            && !found.addresses.iter().any(|a| normalised(a) == *at)
            && let Some(ip) = ips.iter().copied().find(|ip| normalised(&typed(*ip, port)) == *at)
        {
            found.addresses.insert(0, typed(ip, port));
        }
        if self.heard.get(fullname).is_some_and(|h| h.found == found && h.host == host && h.ips == ips) {
            return false;
        }
        // Counted without this server's own earlier announcement.
        let others = || self.heard.iter().filter(|(name, _)| name.as_str() != fullname).map(|(_, h)| h);
        let admitted = match &trusted_at {
            // Once per trusted address.
            Some(at) => !others().any(|h| h.trusted_at.as_ref() == Some(at)),
            None if others().filter(|h| h.trusted_at.is_none()).count() >= MAX_FOUND => false,
            None => {
                let untrusted = || others().filter(|h| h.trusted_at.is_none());
                let same_host = untrusted().filter(|h| h.host == host).count();
                let same_address = ips.iter().map(|ip| untrusted().filter(|h| h.ips.contains(ip)).count()).max().unwrap_or(0);
                same_host < MAX_PER_HOST && same_address < MAX_PER_HOST
            }
        };
        if !admitted {
            // Not taken in (an earlier announcement of it stays as it was).
            return false;
        }
        self.heard.insert(fullname.to_string(), Heard { found, host, ips, port, trusted_at });
        true
    }

    /// Forgets a server (it said goodbye). Returns whether the list changed.
    pub fn removed(&mut self, fullname: &str) -> bool {
        self.heard.remove(fullname).is_some()
    }

    /// The servers, those already trusted first, then by name.
    pub fn list(&self) -> Vec<Found> {
        let mut list: Vec<&Heard> = self.heard.values().collect();
        list.sort_by(|a, b| {
            b.trusted_at
                .is_some()
                .cmp(&a.trusted_at.is_some())
                .then_with(|| a.found.name.to_lowercase().cmp(&b.found.name.to_lowercase()))
                .then_with(|| a.found.address.cmp(&b.found.address))
        });
        list.into_iter().map(|h| h.found.clone()).collect()
    }
}

/// Listens for servers' announcements until dropped.
pub struct Browser {
    listener: mdns::Listener,
    /// The listener's news last taken in.
    seen: Option<u64>,
    found: FoundList,
}

impl Browser {
    /// Starts listening on the standard mDNS port.
    pub fn start() -> Result<Browser, String> {
        Self::start_on(MDNS_PORT)
    }

    /// [`Browser::start`] on another mDNS port (tests).
    pub fn start_on(mdns_port: u16) -> Result<Browser, String> {
        let listener = mdns::Listener::start(service_name(), mdns_port, mdns_nets)
            .map_err(|e| format!("cannot look for servers on the network: {e}"))?;
        Ok(Browser { listener, seen: None, found: FoundList::default() })
    }

    /// Called (from another thread) whenever the servers heard of change: no need to look
    /// otherwise.
    pub fn set_waker(&self, waker: std::sync::Arc<dyn Fn() + Send + Sync>) {
        self.listener.set_waker(waker);
    }

    /// Takes in what was heard since the last call; returns whether the list changed.
    pub fn poll(&mut self) -> bool {
        let (generation, resolved) = self.listener.resolved();
        if self.seen == Some(generation) {
            return false;
        }
        self.seen = Some(generation);
        let now: Vec<String> = resolved.iter().map(|r| fullname_of(&r.instance)).collect();
        let gone: Vec<String> = self.found.heard.keys().filter(|k| !now.contains(k)).cloned().collect();
        let mut changed = false;
        for k in gone {
            changed |= self.found.removed(&k);
        }
        for (r, fullname) in resolved.iter().zip(&now) {
            changed |= self.found.heard(fullname, &r.host, r.port, &r.addresses);
        }
        changed
    }

    /// The servers heard of: those at an address already trusted first, then by name.
    pub fn found(&self) -> Vec<Found> {
        self.found.list()
    }

    /// The addresses whose key is trusted already: their servers always get in, and come first.
    pub fn set_trusted<'a>(&mut self, addresses: impl IntoIterator<Item = &'a str>) {
        self.found.set_trusted(addresses);
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
        // Loopback from the network would point at whoever connects: never offered.
        assert_eq!(pick_address(&[local6, lo], DEFAULT_PORT), None);
        assert_eq!(pick_address(&[IpAddr::V6(Ipv6Addr::LOCALHOST)], DEFAULT_PORT), None);
        assert_eq!(pick_address(&[local6], DEFAULT_PORT), None);
        assert_eq!(pick_address(&[lan], 0), None);
        assert_eq!(pick_address(&[], DEFAULT_PORT), None);
        assert_eq!(pick_address(&[IpAddr::V4(Ipv4Addr::UNSPECIFIED), IpAddr::V4(Ipv4Addr::new(224, 0, 0, 251))], DEFAULT_PORT), None);
        // Every address picked is one the Open from Server window takes.
        for a in [pick_address(&[lan], 47400), pick_address(&[global6], DEFAULT_PORT), pick_address(&[global6], 9)] {
            assert!(crate::link::normalise_address(&a.unwrap()).is_ok());
        }
    }

    /// Regression: one computer announcing many names filled the list (64, first come), so
    /// servers heard of later never showed.
    #[test]
    fn one_computer_cannot_fill_the_list() {
        let ip = |n: u8| IpAddr::V4(Ipv4Addr::new(192, 168, 1, n));
        let name = |i: usize| format!("Flood {i}._plancraft._tcp.local.");
        let mut list = FoundList::default();
        // One host name, one address: four names at most.
        for i in 0..100 {
            list.heard(&name(i), "flood.local.", DEFAULT_PORT, &[ip(66)]);
        }
        assert_eq!(list.list().len(), MAX_PER_HOST);
        // Other host names but the same address: still that address's four.
        for i in 100..200 {
            list.heard(&name(i), &format!("flood-{i}.local."), DEFAULT_PORT, &[ip(66)]);
        }
        assert_eq!(list.list().len(), MAX_PER_HOST);
        // One host name, many addresses: still that host's four.
        for i in 200..250u8 {
            list.heard(&name(usize::from(i)), "flood.local.", DEFAULT_PORT, &[ip(i)]);
        }
        assert_eq!(list.list().len(), MAX_PER_HOST);
        // One address on many ports: still that address's four.
        for i in 250..300u16 {
            list.heard(&name(usize::from(i)), &format!("p{i}.local."), 50_000 + i, &[ip(66)]);
        }
        assert_eq!(list.list().len(), MAX_PER_HOST);
        // A real server heard afterwards gets in.
        assert!(list.heard("Plans on studio-pc._plancraft._tcp.local.", "studio-pc-plancraft.local.", DEFAULT_PORT, &[ip(20)]));
        assert!(list.list().iter().any(|f| f.name == "Plans on studio-pc"));
        // A server saying goodbye leaves; a changed announcement of one listed is taken in.
        assert!(list.heard("Plans on studio-pc._plancraft._tcp.local.", "studio-pc-plancraft.local.", 47400, &[ip(20)]));
        assert!(list.list().iter().any(|f| f.address == "192.168.1.20:47400"));
        assert!(list.removed("Plans on studio-pc._plancraft._tcp.local."));
        assert!(!list.list().iter().any(|f| f.name == "Plans on studio-pc"));
    }

    #[test]
    fn many_computers_fill_the_list_but_trusted_servers_still_get_in_first() {
        let mut list = FoundList::default();
        list.set_trusted(["10.0.0.9", "office-pc"]);
        for i in 0..200u16 {
            let [a, b] = i.to_be_bytes();
            let addr = IpAddr::V4(Ipv4Addr::new(10, 1, a, b));
            list.heard(&format!("Z{i}._plancraft._tcp.local."), &format!("h{i}.local."), DEFAULT_PORT, &[addr]);
        }
        assert_eq!(list.list().len(), MAX_FOUND);
        let trusted = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 9));
        assert!(list.heard("Office._plancraft._tcp.local.", "office.local.", DEFAULT_PORT, &[trusted]));
        let first = list.list().into_iter().next().unwrap();
        assert_eq!((first.name.as_str(), first.address.as_str()), ("Office", "10.0.0.9"));
        assert_eq!(list.list().len(), MAX_FOUND + 1);
        // Others claiming the trusted address (anyone can add it to an announcement) are not
        // taken in: one server per trusted address.
        for i in 0..64 {
            assert!(!list.heard(&format!("Payroll {i}._plancraft._tcp.local."), "evil.local.", DEFAULT_PORT, &[trusted]));
        }
        assert_eq!(list.list().len(), MAX_FOUND + 1);
        // A trusted address is found among many, however many better ones come before it.
        let mut list = FoundList::default();
        list.set_trusted(["192.168.1.9"]);
        let mut many: Vec<IpAddr> = (1..=20).map(|n| IpAddr::V4(Ipv4Addr::new(1, 0, 0, n))).collect();
        many.push(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 9)));
        assert!(list.heard("Plans._plancraft._tcp.local.", "pc.local.", DEFAULT_PORT, &many));
        let f = list.list().into_iter().next().unwrap();
        assert!(f.addresses.contains(&"192.168.1.9".to_string()), "{:?}", f.addresses);
        // Trusting it later moves it up too.
        let mut list = FoundList::default();
        list.heard("B._plancraft._tcp.local.", "b.local.", DEFAULT_PORT, &[IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2))]);
        list.heard("A._plancraft._tcp.local.", "a.local.", DEFAULT_PORT, &[IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))]);
        assert_eq!(list.list().first().unwrap().name, "A");
        list.set_trusted(["10.0.0.2"]);
        assert_eq!(list.list().first().unwrap().name, "B");
    }

    #[test]
    fn vpns_and_overlays_are_not_announced_on() {
        for name in ["tun0", "utun3", "wg0", "tailscale0", "ztabcdef", "ppp0"] {
            assert!(overlay_interface(name), "{name}");
        }
        for name in ["eth0", "en0", "Wi-Fi", "wlan0"] {
            assert!(!overlay_interface(name), "{name}");
        }
    }

    #[test]
    fn container_bridges_are_not_offered() {
        for name in ["docker0", "br-3f2a", "veth12", "virbr0", "vboxnet0", "vmnet8", "podman0"] {
            assert!(virtual_interface(name), "{name}");
        }
        for name in [
            "vEthernet (WSL (Hyper-V firewall))",
            "vEthernet (Default Switch)",
            "bridge100",
            "vnic0",
            "VirtualBox Host-Only Network",
            "VMware Network Adapter VMnet8",
        ] {
            assert!(virtual_interface(name), "{name}");
        }
        // A Hyper-V external switch holds the computer's real address.
        for name in ["eth0", "enp5s0", "wlan0", "en0", "Ethernet", "Wi-Fi", "tailscale0", "wg0", "vEthernet (External Switch)", "bridge0"] {
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
        // Blank-looking characters in front of a mark, marks anywhere, combining marks.
        for fake in ["\u{3164}✔ Plans", "\u{2800}✓Plans", "Pla√ns", "\u{e0041}🗸 Pl\u{0301}ans", "\u{ffa0}\u{fe0f}✅Plans"] {
            let f = found_from(&format!("{fake}._plancraft._tcp.local."), DEFAULT_PORT, &lan).unwrap();
            assert_eq!(f.name, "Plans", "{fake:?}");
        }
        // Every address is kept, best first: one already trusted can be picked over the best.
        let many = [IpAddr::V4(Ipv4Addr::new(192, 168, 1, 9)), IpAddr::V4(Ipv4Addr::new(1, 0, 0, 1)), IpAddr::V4(Ipv4Addr::LOCALHOST)];
        let f = found_from("x._plancraft._tcp.local.", DEFAULT_PORT, &many).unwrap();
        assert_eq!(f.addresses, ["1.0.0.1", "192.168.1.9"]);
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
