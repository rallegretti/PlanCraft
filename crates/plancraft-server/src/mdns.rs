//! A small Multicast DNS responder and listener for PlanCraft's one service (RFC 6762, DNS-SD
//! RFC 6763), in pure Rust on the standard library's sockets (and `socket2` for the socket options
//! it lacks). It replaces a general mDNS library that kept every record anyone sent, with no limit
//! on how many or for how long, so a hostile computer on the network could fill memory.
//!
//! What it does, and no more:
//!
//! - **Responder** ([`Responder`]): answers questions about one PlanCraft server (its PTR, SRV,
//!   TXT and A records), announces it when it starts and on each network it joins, and says
//!   goodbye when it stops. It keeps nothing it hears.
//! - **Listener** ([`Listener`]): asks for PlanCraft servers and keeps only their records
//!   ([`Heard`]). Each server belongs to the computer that announced it (the packet's sender):
//!   only that computer can change it, point it elsewhere, give its addresses or say goodbye for
//!   it, and one computer holds at most [`MAX_PER_SENDER`] servers. Everything is bounded and
//!   forgotten in time ([`MAX_KEPT`]).
//! - Both hear only computers on a network this one is on: a packet whose sender is outside every
//!   local network (an IPv4 address and its netmask) is dropped unread (RFC 6762 §11); the
//!   listener also takes answers only from the mDNS port (§6).
//! - Packets are read with every length, count, label and compression pointer checked, and at
//!   most [`MAX_NAME_BYTES_PER_PACKET`] bytes of names decoded: nothing a packet says can make it
//!   panic, loop or cost much (`tests/mdns.rs` sweeps random and mDNS-like packets).
//!
//! IPv4 only (224.0.0.251): servers announce their IPv4 addresses.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The mDNS multicast group.
pub const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
/// The standard mDNS port. The listener takes answers only from the mDNS port it uses (RFC 6762
/// §6: answers from other ports are not mDNS).
pub const MDNS_PORT: u16 = 5353;
/// Largest packet read or sent (RFC 6762 §17: 9000 bytes).
const MAX_PACKET: usize = 9000;
/// Questions and records read from one packet, at most (the rest is ignored).
const MAX_QUESTIONS: usize = 64;
const MAX_RECORDS: usize = 256;
/// Compression pointers followed in one name, at most.
const MAX_JUMPS: usize = 64;
/// Bytes of names decoded from one packet, at most (a packet of long names pointing at each other
/// costs no more than this).
pub const MAX_NAME_BYTES_PER_PACKET: usize = 4096;
/// PlanCraft servers kept by a listener, at most.
pub const MAX_INSTANCES: usize = 256;
/// Servers kept per sending computer, at most: one computer can't crowd out the others.
pub const MAX_PER_SENDER: usize = 8;
/// Addresses kept per server, at most.
pub const MAX_ADDRESSES: usize = 8;
/// Longest a listener keeps a record without hearing it again, whatever lifetime it was sent
/// with (it asks again every [`MAX_QUERY_GAP`] at most, so a server answering stays).
pub const MAX_KEPT: Duration = Duration::from_secs(180);
/// How long a server heard of but not yet told where it is (no SRV, or no address) is kept.
pub const UNRESOLVED_KEPT: Duration = Duration::from_secs(20);
/// Longest wait between a listener's questions.
pub const MAX_QUERY_GAP: Duration = Duration::from_secs(30);
/// A goodbye takes effect after this, unless the server answers meanwhile (RFC 6762 §10.1).
const GOODBYE_GRACE: Duration = Duration::from_secs(1);
/// Lifetimes of the records a responder sends (RFC 6762 §10).
const HOST_TTL: u32 = 120;
const OTHER_TTL: u32 = 4500;
/// How often the computer's networks are looked at again.
const RESCAN: Duration = Duration::from_secs(5);
/// How long a socket read waits, so stopping and timers are seen.
const TICK: Duration = Duration::from_millis(100);
/// A responder answers on one network at most this often (RFC 6762 §6).
const ANSWER_GAP: Duration = Duration::from_secs(1);
/// Unicast answers (to one-shot queriers) per second in all, at most.
const MAX_UNICAST_PER_SECOND: u32 = 20;
/// The listener tells about changes at most this often.
const WAKE_GAP: Duration = Duration::from_millis(250);

const TYPE_A: u16 = 1;
const TYPE_PTR: u16 = 12;
const TYPE_TXT: u16 = 16;
const TYPE_AAAA: u16 = 28;
const TYPE_SRV: u16 = 33;
const TYPE_ANY: u16 = 255;
const CLASS_IN: u16 = 1;
/// The top bit of a record's class: "this replaces what you have" (RFC 6762 §10.2).
const CACHE_FLUSH: u16 = 0x8000;
/// The top bit of a question's class: "answer me directly" (RFC 6762 §5.4).
const UNICAST_RESPONSE: u16 = 0x8000;
/// The name DNS-SD browsers ask to list every kind of service (RFC 6763 §9).
const SERVICES_META: &str = "_services._dns-sd._udp.local.";

fn later(now: Instant, by: Duration) -> Instant {
    now.checked_add(by).unwrap_or(now)
}

// ---------------------------------------------------------------------------------------------
// Names

/// A domain name as its labels (raw bytes; compared ignoring ASCII case).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Name(pub Vec<Vec<u8>>);

impl Name {
    /// A name from dotted text with no escapes (`_plancraft._tcp.local.`).
    pub fn dotted(text: &str) -> Name {
        Name(text.split('.').filter(|l| !l.is_empty()).map(|l| l.as_bytes().to_vec()).collect())
    }

    /// `first` in front of `rest`.
    pub fn child(first: &[u8], rest: &Name) -> Name {
        let mut labels = vec![first.to_vec()];
        labels.extend(rest.0.iter().cloned());
        Name(labels)
    }

    /// The same name, ignoring ASCII case.
    pub fn same(&self, other: &Name) -> bool {
        self.0.len() == other.0.len() && self.0.iter().zip(&other.0).all(|(a, b)| a.eq_ignore_ascii_case(b))
    }

    /// This name is `parent` with one label in front: that label.
    pub fn child_of(&self, parent: &Name) -> Option<&[u8]> {
        let (first, rest) = self.0.split_first()?;
        (rest.len() == parent.0.len() && rest.iter().zip(&parent.0).all(|(a, b)| a.eq_ignore_ascii_case(b))).then_some(first.as_slice())
    }

    /// As text: lower case (ASCII), labels escaped, dots between and at the end
    /// (`studio-pc.local.`).
    pub fn text(&self) -> String {
        let mut out = String::new();
        for label in &self.0 {
            out.push_str(&escape_label(&label.to_ascii_lowercase()));
            out.push('.');
        }
        out
    }
}

/// A label as text, `.` and `\` escaped with `\`; bytes that are not UTF-8 replaced.
pub fn escape_label(label: &[u8]) -> String {
    String::from_utf8_lossy(label).replace('\\', "\\\\").replace('.', "\\.")
}

// ---------------------------------------------------------------------------------------------
// Reading packets

/// A question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    pub name: Name,
    pub qtype: u16,
}

/// What a record says (only the kinds PlanCraft looks at).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Data {
    A(Ipv4Addr),
    Aaaa(std::net::Ipv6Addr),
    Ptr(Name),
    Srv { port: u16, target: Name },
    Other,
}

/// A record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub name: Name,
    pub rtype: u16,
    pub ttl: u32,
    /// It replaces what the receiver has for this name and type (RFC 6762 §10.2).
    pub flush: bool,
    pub data: Data,
}

/// A packet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Message {
    pub id: u16,
    pub response: bool,
    pub questions: Vec<Question>,
    /// Answers, authority and additional records together.
    pub records: Vec<Record>,
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    /// Bytes of names this packet may still have decoded.
    budget: usize,
}

impl Reader<'_> {
    fn u8(&mut self) -> Option<u8> {
        let b = *self.buf.get(self.pos)?;
        self.pos = self.pos.checked_add(1)?;
        Some(b)
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes([self.u8()?, self.u8()?]))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes([self.u8()?, self.u8()?, self.u8()?, self.u8()?]))
    }

    fn skip(&mut self, n: usize) -> Option<()> {
        let end = self.pos.checked_add(n)?;
        if end > self.buf.len() {
            return None;
        }
        self.pos = end;
        Some(())
    }

    /// Steps over a name without reading it.
    fn skip_name(&mut self) -> Option<()> {
        for _ in 0..128 {
            let len = self.u8()?;
            match len & 0xC0 {
                0x00 if len == 0 => return Some(()),
                0x00 => self.skip(usize::from(len))?,
                0xC0 => return self.u8().map(|_| ()),
                _ => return None,
            }
        }
        None
    }

    /// A name, following compression pointers (only backwards, at most [`MAX_JUMPS`]; at most
    /// 255 bytes in all, labels of at most 63; within the packet's budget).
    fn name(&mut self) -> Option<Name> {
        let mut labels = Vec::new();
        // The final zero counts too (RFC 1035: 255 bytes in all).
        let mut total = 1usize;
        let mut at = self.pos;
        let mut jumped = false;
        let mut jumps = 0usize;
        loop {
            let len = *self.buf.get(at)?;
            match len & 0xC0 {
                0x00 if len == 0 => {
                    if !jumped {
                        self.pos = at.checked_add(1)?;
                    }
                    return Some(Name(labels));
                }
                0x00 => {
                    let start = at.checked_add(1)?;
                    let end = start.checked_add(usize::from(len))?;
                    let label = self.buf.get(start..end)?;
                    total = total.checked_add(usize::from(len))?.checked_add(1)?;
                    self.budget = self.budget.checked_sub(usize::from(len).saturating_add(1))?;
                    if total > 255 {
                        return None;
                    }
                    labels.push(label.to_vec());
                    at = end;
                }
                0xC0 => {
                    let low = *self.buf.get(at.checked_add(1)?)?;
                    let target = usize::from(u16::from_be_bytes([len & 0x3F, low]));
                    // Only backwards: a pointer forwards or to itself could go round for ever.
                    if target >= at || jumps >= MAX_JUMPS {
                        return None;
                    }
                    if !jumped {
                        self.pos = at.checked_add(2)?;
                    }
                    jumped = true;
                    jumps = jumps.saturating_add(1);
                    self.budget = self.budget.checked_sub(1)?;
                    at = target;
                }
                _ => return None,
            }
        }
    }
}

/// What to read of a packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// Its questions, if it is a query (a responder's need).
    Questions,
    /// Its records, if it is a response (a listener's need).
    Records,
    /// Both (tests).
    All,
}

/// Reads a packet in full. `None` when it is not one.
pub fn parse(buf: &[u8]) -> Option<Message> {
    parse_part(buf, Part::All)
}

/// Reads the `part` of a packet that matters (a query's questions, a response's records); `None`
/// when it is not one or is the other kind: nothing a packet holds can make this panic or take
/// long.
pub fn parse_part(buf: &[u8], part: Part) -> Option<Message> {
    let mut r = Reader { buf, pos: 0, budget: MAX_NAME_BYTES_PER_PACKET };
    let id = r.u16()?;
    let flags = r.u16()?;
    let qd = usize::from(r.u16()?);
    let an = usize::from(r.u16()?);
    let ns = usize::from(r.u16()?);
    let ar = usize::from(r.u16()?);
    // Opcode 0 (a standard query or response), no error.
    if flags & 0x7800 != 0 || flags & 0x000F != 0 {
        return None;
    }
    let response = flags & 0x8000 != 0;
    match part {
        Part::Questions if response => return None,
        Part::Records if !response => return None,
        _ => {}
    }
    let mut m = Message { id, response, ..Message::default() };
    for _ in 0..qd.min(MAX_QUESTIONS) {
        if part == Part::Records {
            r.skip_name()?;
            r.skip(4)?;
            continue;
        }
        let name = r.name()?;
        let qtype = r.u16()?;
        let _class = r.u16()? & !UNICAST_RESPONSE;
        m.questions.push(Question { name, qtype });
    }
    if qd > MAX_QUESTIONS || part == Part::Questions {
        return Some(m);
    }
    let records = an.saturating_add(ns).saturating_add(ar);
    for _ in 0..records.min(MAX_RECORDS) {
        let name = r.name()?;
        let rtype = r.u16()?;
        let class_bits = r.u16()?;
        let (class, flush) = (class_bits & !CACHE_FLUSH, class_bits & CACHE_FLUSH != 0);
        let ttl = r.u32()?;
        let len = usize::from(r.u16()?);
        let start = r.pos;
        let end = start.checked_add(len)?;
        let rdata = buf.get(start..end)?;
        let data = match (rtype, class) {
            (TYPE_A, CLASS_IN) => match rdata {
                [a, b, c, d] => Data::A(Ipv4Addr::new(*a, *b, *c, *d)),
                _ => Data::Other,
            },
            (TYPE_AAAA, CLASS_IN) => <[u8; 16]>::try_from(rdata).map(|o| Data::Aaaa(o.into())).unwrap_or(Data::Other),
            (TYPE_PTR, CLASS_IN) => match r.name() {
                Some(n) if r.pos <= end => Data::Ptr(n),
                _ => Data::Other,
            },
            (TYPE_SRV, CLASS_IN) if len >= 7 => {
                let fields = (r.u16(), r.u16(), r.u16());
                match (fields, r.name()) {
                    ((Some(_), Some(_), Some(port)), Some(target)) if r.pos <= end => Data::Srv { port, target },
                    _ => Data::Other,
                }
            }
            _ => Data::Other,
        };
        r.pos = end;
        m.records.push(Record { name, rtype, ttl, flush, data });
    }
    Some(m)
}

// ---------------------------------------------------------------------------------------------
// Writing packets

struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn new(id: u16, flags: u16, questions: usize, answers: usize) -> Writer {
        let mut w = Writer { buf: Vec::with_capacity(512) };
        for v in [id, flags, count(questions), count(answers), 0, 0] {
            w.buf.extend_from_slice(&v.to_be_bytes());
        }
        w
    }

    fn name(&mut self, name: &Name) {
        for label in &name.0 {
            let label = label.get(..63).unwrap_or(label);
            self.buf.push(u8::try_from(label.len()).unwrap_or(63));
            self.buf.extend_from_slice(label);
        }
        self.buf.push(0);
    }

    fn question(&mut self, q: &Question) {
        self.name(&q.name);
        self.buf.extend_from_slice(&q.qtype.to_be_bytes());
        self.buf.extend_from_slice(&CLASS_IN.to_be_bytes());
    }

    fn record(&mut self, name: &Name, rtype: u16, flush: bool, ttl: u32, rdata: &[u8]) {
        self.name(name);
        self.buf.extend_from_slice(&rtype.to_be_bytes());
        let class = if flush { CLASS_IN | CACHE_FLUSH } else { CLASS_IN };
        self.buf.extend_from_slice(&class.to_be_bytes());
        self.buf.extend_from_slice(&ttl.to_be_bytes());
        self.buf.extend_from_slice(&count(rdata.len()).to_be_bytes());
        self.buf.extend_from_slice(rdata);
    }
}

fn count(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

fn name_bytes(name: &Name) -> Vec<u8> {
    let mut w = Writer { buf: Vec::new() };
    w.name(name);
    w.buf
}

/// A query for `questions` (multicast answers wanted).
pub fn query(questions: &[Question]) -> Vec<u8> {
    let mut w = Writer::new(0, 0, questions.len(), 0);
    for q in questions {
        w.question(q);
    }
    w.buf
}

// ---------------------------------------------------------------------------------------------
// Networks

/// A network this computer is on: its IPv4 address there, the netmask, and the interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Net {
    pub ip: Ipv4Addr,
    pub mask: Ipv4Addr,
    /// The interface's index, if known (two addresses on one interface share its membership).
    pub iface: Option<u32>,
}

impl Net {
    /// The network for `ip` with a `prefix`-bit netmask; `None` for prefixes that would let in
    /// (almost) everyone, or are not prefixes (below 8, above 32).
    pub fn new(ip: Ipv4Addr, prefix: u8, iface: Option<u32>) -> Option<Net> {
        if !(8..=32).contains(&prefix) {
            return None;
        }
        let mask = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
        Some(Net { ip, mask: Ipv4Addr::from(mask), iface })
    }

    /// Whether a computer at `ip` is on this network (not its network or broadcast address, nor
    /// an address no one sends from).
    pub fn holds(&self, ip: Ipv4Addr) -> bool {
        let mask = u32::from(self.mask);
        if ip.is_unspecified() || ip.is_broadcast() || ip.is_multicast() || ip.is_loopback() {
            return false;
        }
        let host = u32::from(ip) & !mask;
        // /31 and /32 have no network or broadcast address.
        let small = mask.count_zeros() <= 1;
        u32::from(ip) & mask == u32::from(self.ip) & mask && (small || (host != 0 && host != !mask))
    }
}

/// The network a packet from `from` came over, if it is on one of `nets` (else it is dropped).
pub fn net_of(nets: &[Net], from: Ipv4Addr) -> Option<Net> {
    nets.iter().copied().find(|n| n.holds(from))
}

/// Opens the mDNS socket on `port`, sharing it with the system's own responder.
fn open(port: u16) -> std::io::Result<UdpSocket> {
    let s = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, Some(socket2::Protocol::UDP))?;
    s.set_reuse_address(true)?;
    #[cfg(unix)]
    {
        // Not everywhere (the system's responder may not use it); reusing the address is enough.
        let _ = s.set_reuse_port(true);
    }
    #[cfg(target_os = "linux")]
    {
        // Only the networks this socket joined, not every one any program on the computer did.
        let _ = s.set_multicast_all_v4(false);
    }
    s.bind(&SocketAddr::from(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port)).into())?;
    // RFC 6762 §11: everything sent with IP TTL 255.
    s.set_multicast_ttl_v4(255)?;
    let _ = s.set_ttl_v4(255);
    s.set_multicast_loop_v4(true)?;
    let udp: UdpSocket = s.into();
    udp.set_read_timeout(Some(TICK))?;
    Ok(udp)
}

/// The socket and the networks it listens on.
struct Link {
    udp: UdpSocket,
    port: u16,
    nets: Vec<Net>,
    find_nets: fn() -> Vec<Net>,
    scanned: Instant,
}

impl Link {
    /// Opens the socket; the networks are joined as the computer has them (none yet is fine:
    /// they are looked at again every few seconds).
    fn open(port: u16, find_nets: fn() -> Vec<Net>) -> Result<Link, String> {
        let udp = open(port).map_err(|e| format!("cannot open the mDNS port {port}: {e}"))?;
        let mut link = Link { udp, port, nets: Vec::new(), find_nets, scanned: Instant::now() };
        link.rescan();
        Ok(link)
    }

    /// Looks at the computer's networks again; returns those it joined now.
    fn rescan(&mut self) -> Vec<Net> {
        self.scanned = Instant::now();
        let now = (self.find_nets)();
        let gone: Vec<Net> = self.nets.iter().copied().filter(|n| !now.contains(n)).collect();
        self.nets.retain(|n| now.contains(n));
        for g in gone {
            // Another address on the same interface still uses its membership.
            if !self.nets.iter().any(|n| n.iface.is_some() && n.iface == g.iface) {
                let _ = self.udp.leave_multicast_v4(&GROUP, &g.ip);
            }
        }
        let mut joined = Vec::new();
        for net in now {
            if self.nets.contains(&net) {
                continue;
            }
            match self.udp.join_multicast_v4(&GROUP, &net.ip) {
                Ok(()) => {}
                // Already joined there (a second address on one interface).
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {}
                Err(_) => continue,
            }
            self.nets.push(net);
            joined.push(net);
        }
        joined
    }

    fn send_multicast(&self, net: &Net, packet: &[u8]) {
        if socket2::SockRef::from(&self.udp).set_multicast_if_v4(&net.ip).is_ok() {
            let _ = self.udp.send_to(packet, SocketAddrV4::new(GROUP, self.port));
        }
    }

    /// The next packet from a computer on one of the networks (with that network), if one came
    /// within a moment.
    fn receive(&self, buf: &mut [u8]) -> Option<(usize, SocketAddrV4, Net)> {
        let (n, from) = match self.udp.recv_from(buf) {
            Ok(got) => got,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => return None,
            Err(_) => {
                // Any other error (a closed port's ICMP answer on Windows): not in a tight loop.
                std::thread::sleep(TICK);
                return None;
            }
        };
        let SocketAddr::V4(from) = from else { return None };
        let net = net_of(&self.nets, *from.ip())?;
        Some((n, from, net))
    }
}

// ---------------------------------------------------------------------------------------------
// Responder

/// The server a responder speaks for.
#[derive(Clone, Debug)]
pub struct Service {
    /// The service type (`_plancraft._tcp.local.`).
    pub service: Name,
    /// Its instance label (`Plans on studio-pc`).
    pub instance: Vec<u8>,
    /// The host name it points to (`studio-pc-plancraft.local.`).
    pub host: Name,
    pub port: u16,
    /// TXT strings (`v=3`).
    pub txt: Vec<String>,
    /// Listening on one address only: announced on that network only, with that address.
    pub only: Option<Ipv4Addr>,
}

/// How a responder's records go out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Way {
    /// Announced or answered by multicast: full lifetimes, cache-flush set.
    Multicast,
    /// Answered straight to a one-shot querier (RFC 6762 §6.7): 10-second lifetimes, no
    /// cache-flush (§10.2).
    Unicast,
    /// Goodbye: lifetime 0. Not the host's address, which other servers on this computer share.
    Goodbye,
}

impl Service {
    fn instance_name(&self) -> Name {
        Name::child(&self.instance, &self.service)
    }

    /// Its records on `net`, written into `w` (whose header counts [`Self::count`] of them).
    fn write(&self, w: &mut Writer, net: &Net, how: Way) {
        let (ttl, flush) = match how {
            Way::Multicast => (None, true),
            Way::Unicast => (Some(10), false),
            Way::Goodbye => (Some(0), true),
        };
        let ttl_or = |t: u32| ttl.unwrap_or(t);
        let instance = self.instance_name();
        let mut srv = Vec::new();
        srv.extend_from_slice(&0u16.to_be_bytes());
        srv.extend_from_slice(&0u16.to_be_bytes());
        srv.extend_from_slice(&self.port.to_be_bytes());
        srv.extend_from_slice(&name_bytes(&self.host));
        let mut txt = Vec::new();
        for t in &self.txt {
            let t = t.as_bytes();
            let t = t.get(..255).unwrap_or(t);
            txt.push(u8::try_from(t.len()).unwrap_or(255));
            txt.extend_from_slice(t);
        }
        if txt.is_empty() {
            txt.push(0);
        }
        w.record(&self.service, TYPE_PTR, false, ttl_or(OTHER_TTL), &name_bytes(&instance));
        w.record(&instance, TYPE_SRV, flush, ttl_or(HOST_TTL), &srv);
        w.record(&instance, TYPE_TXT, flush, ttl_or(OTHER_TTL), &txt);
        if how != Way::Goodbye {
            w.record(&self.host, TYPE_A, flush, ttl_or(HOST_TTL), &net.ip.octets());
        }
    }

    /// How many records [`Self::write`] writes.
    fn count(how: Way) -> usize {
        if how == Way::Goodbye { 3 } else { 4 }
    }

    /// Its records on `net`, announced (or goodbye).
    fn records(&self, net: &Net, how: Way) -> Vec<u8> {
        let mut w = Writer::new(0, 0x8400, 0, Self::count(how));
        self.write(&mut w, net, how);
        w.buf
    }

    /// An answer by multicast; with the DNS-SD listing of service types when that was asked.
    fn answer(&self, net: &Net, listing: bool) -> Vec<u8> {
        let mut w = Writer::new(0, 0x8400, 0, Self::count(Way::Multicast).saturating_add(usize::from(listing)));
        self.write(&mut w, net, Way::Multicast);
        if listing {
            w.record(&Name::dotted(SERVICES_META), TYPE_PTR, false, OTHER_TTL, &name_bytes(&self.service));
        }
        w.buf
    }

    /// Whether `q` asks about this server.
    fn asked(&self, q: &Question) -> bool {
        let any = q.qtype == TYPE_ANY;
        (q.name.same(&self.service) && (any || q.qtype == TYPE_PTR))
            || (q.name.same(&self.instance_name()) && (any || q.qtype == TYPE_SRV || q.qtype == TYPE_TXT))
            || (q.name.same(&self.host) && (any || q.qtype == TYPE_A))
    }

    /// Whether `q` asks for the list of service types (RFC 6763 §9).
    fn asked_listing(q: &Question) -> bool {
        q.name.same(&Name::dotted(SERVICES_META)) && (q.qtype == TYPE_PTR || q.qtype == TYPE_ANY)
    }

    /// An answer straight to a one-shot querier (RFC 6762 §6.7): its id and its questions about
    /// this server back (only those), short lifetimes, no cache-flush.
    fn unicast_answer(&self, net: &Net, query: &Message) -> Vec<u8> {
        let ours: Vec<&Question> = query.questions.iter().filter(|q| self.asked(q)).take(4).collect();
        let mut w = Writer::new(query.id, 0x8400, ours.len(), Self::count(Way::Unicast));
        for q in ours {
            w.question(q);
        }
        self.write(&mut w, net, Way::Unicast);
        w.buf
    }
}

/// Announces a server and answers for it until dropped (then says goodbye).
pub struct Responder {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Responder {
    /// Starts answering for `service` on mDNS `port`, on the networks `find_nets` gives (looked at
    /// again every few seconds; announced on as they are joined).
    pub fn start(service: Service, port: u16, find_nets: fn() -> Vec<Net>) -> Result<Responder, String> {
        let mut link = Link::open(port, find_nets)?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("plancraft-mdns-responder".into())
            .spawn(move || respond(&mut link, &service, &stopped))
            .map_err(|e| format!("cannot announce the server: {e}"))?;
        Ok(Responder { stop, thread: Some(thread) })
    }
}

impl Drop for Responder {
    /// Says goodbye on every network (lists drop the server at once) and stops.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A small delay, different each time, for answers many servers give at once (RFC 6762 §6:
/// 20–120 ms), so they don't all answer at the same instant.
fn jitter() -> Duration {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    Duration::from_millis(20u64.saturating_add(u64::from(nanos % 101)))
}

/// Whether `net` is one this responder speaks on.
fn speaks_on(service: &Service, net: &Net) -> bool {
    service.only.is_none_or(|ip| ip == net.ip)
}

fn respond(link: &mut Link, service: &Service, stop: &AtomicBool) {
    // Announced three times, a second apart (RFC 6762 §8.3), on each network as it is joined.
    let mut announce: Vec<(Net, Instant, u8)> =
        link.nets.iter().filter(|n| speaks_on(service, n)).map(|n| (*n, Instant::now(), 0)).collect();
    // Per network: when it was last answered, and an answer waiting to go (and whether it lists
    // the service types).
    let mut answered: BTreeMap<u32, Instant> = BTreeMap::new();
    let mut waiting: BTreeMap<u32, (Instant, bool)> = BTreeMap::new();
    // One-shot queriers: answers this second in all, and per querier.
    let mut unicast: (Instant, u32, BTreeMap<Ipv4Addr, u32>) = (Instant::now(), 0, BTreeMap::new());
    let mut buf = vec![0u8; MAX_PACKET];
    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        for (net, next, sent) in &mut announce {
            if *sent < 3 && now >= *next {
                link.send_multicast(net, &service.records(net, Way::Multicast));
                *sent = sent.saturating_add(1);
                *next = later(now, Duration::from_secs(1));
                answered.insert(u32::from(net.ip), now);
            }
        }
        announce.retain(|(_, _, sent)| *sent < 3);
        let due: Vec<u32> = waiting.iter().filter(|(_, (at, _))| *at <= now).map(|(ip, _)| *ip).collect();
        for ip in due {
            if let (Some((_, listing)), Some(net)) = (waiting.remove(&ip), link.nets.iter().find(|n| u32::from(n.ip) == ip)) {
                link.send_multicast(net, &service.answer(net, listing));
                answered.insert(ip, Instant::now());
            }
        }
        if now.duration_since(link.scanned) >= RESCAN {
            for net in link.rescan() {
                if speaks_on(service, &net) {
                    announce.push((net, Instant::now(), 0));
                }
            }
            answered.retain(|ip, _| link.nets.iter().any(|n| u32::from(n.ip) == *ip));
            waiting.retain(|ip, _| link.nets.iter().any(|n| u32::from(n.ip) == *ip));
        }
        let Some((n, from, net)) = link.receive(&mut buf) else { continue };
        if !speaks_on(service, &net) {
            continue;
        }
        let Some(m) = buf.get(..n).and_then(|p| parse_part(p, Part::Questions)) else { continue };
        let ours = m.questions.iter().any(|q| service.asked(q));
        let listing = m.questions.iter().any(Service::asked_listing);
        if !ours && !listing {
            continue;
        }
        if from.port() != link.port {
            // A one-shot querier (RFC 6762 §6.7): answered directly, a few a second at most, in
            // all and per querier; never to this computer's own addresses.
            if !ours || link.nets.iter().any(|n| n.ip == *from.ip()) {
                continue;
            }
            if unicast.0.elapsed() >= Duration::from_secs(1) {
                unicast = (Instant::now(), 0, BTreeMap::new());
            }
            let per = unicast.2.entry(*from.ip()).or_insert(0);
            if unicast.1 < MAX_UNICAST_PER_SECOND && *per < 2 {
                *per = per.saturating_add(1);
                unicast.1 = unicast.1.saturating_add(1);
                let _ = link.udp.send_to(&service.unicast_answer(&net, &m), from);
            }
            continue;
        }
        // By multicast, after a short random delay, and a second after the last answer on that
        // network at the soonest: put off, not dropped.
        let ip = u32::from(net.ip);
        let soonest = answered.get(&ip).map_or(now, |t| later(*t, ANSWER_GAP));
        let at = later(now, jitter()).max(soonest);
        let entry = waiting.entry(ip).or_insert((at, false));
        entry.1 |= listing;
    }
    for net in link.nets.iter().filter(|n| speaks_on(service, n)) {
        link.send_multicast(net, &service.records(net, Way::Goodbye));
    }
}

// ---------------------------------------------------------------------------------------------
// Listener

/// A PlanCraft server a listener has all it needs for: its instance label, the host name it
/// points to (text), its port and addresses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub instance: Vec<u8>,
    pub host: String,
    pub port: u16,
    pub addresses: Vec<IpAddr>,
}

#[derive(Debug)]
struct Instance {
    label: Vec<u8>,
    /// The computer that announced it: only it can change it.
    owner: Ipv4Addr,
    /// When it is forgotten unless heard again (sooner while it is not resolved).
    expires: Instant,
    /// How long it is kept once heard (its PTR lifetime, capped).
    kept: Duration,
    /// Where it is: host name, port, and until when.
    srv: Option<(Name, u16, Instant)>,
    /// Its addresses (from its owner, for its host name): address, until when, when heard.
    addresses: Vec<(IpAddr, Instant, Instant)>,
}

impl Instance {
    fn resolved(&self) -> bool {
        self.srv.is_some() && !self.addresses.is_empty()
    }
}

/// What a listener heard, bounded: PlanCraft servers only, [`MAX_PER_SENDER`] per computer,
/// [`MAX_INSTANCES`] in all, [`MAX_ADDRESSES`] addresses each, nothing kept longer than
/// [`MAX_KEPT`] without being heard again.
#[derive(Debug, Default)]
pub struct Heard {
    service: Name,
    /// By instance label, lower case.
    instances: BTreeMap<Vec<u8>, Instance>,
    /// Something was put in doubt (a goodbye): ask again at once.
    reconfirm: bool,
}

fn kept_for(ttl: u32) -> Duration {
    Duration::from_secs(u64::from(ttl)).min(MAX_KEPT)
}

impl Heard {
    pub fn new(service: Name) -> Heard {
        Heard { service, ..Heard::default() }
    }

    /// Takes in a response from the computer at `from` (its records about PlanCraft servers it
    /// announced; others are not kept). Returns whether anything changed. Lifetime 0 is a goodbye,
    /// taking effect after a second unless the server answers meanwhile.
    pub fn take(&mut self, m: &Message, from: Ipv4Addr, now: Instant) -> bool {
        let mut changed = false;
        // Servers first, then where they point, then their addresses (any order in the packet).
        for r in &m.records {
            let (TYPE_PTR, Data::Ptr(target)) = (r.rtype, &r.data) else { continue };
            if !r.name.same(&self.service) {
                continue;
            }
            let Some(label) = target.child_of(&self.service) else { continue };
            let key = label.to_ascii_lowercase();
            if let Some(i) = self.instances.get_mut(&key) {
                if i.owner != from {
                    // Someone else's server: not theirs to change.
                    continue;
                }
                if r.ttl == 0 {
                    i.expires = later(now, GOODBYE_GRACE).min(i.expires);
                    self.reconfirm = true;
                } else {
                    i.kept = kept_for(r.ttl);
                    i.expires = later(now, if i.resolved() { i.kept } else { i.kept.min(UNRESOLVED_KEPT) });
                }
                continue;
            }
            if r.ttl == 0 || self.instances.values().filter(|i| i.owner == from).count() >= MAX_PER_SENDER {
                continue;
            }
            if self.instances.len() >= MAX_INSTANCES {
                // Full: room is made only by forgetting one not resolved yet (the soonest to go).
                let evict = self.instances.iter().filter(|(_, i)| !i.resolved()).min_by_key(|(_, i)| i.expires).map(|(k, _)| k.clone());
                let Some(evict) = evict else { continue };
                self.instances.remove(&evict);
            }
            let kept = kept_for(r.ttl);
            let fresh = Instance {
                label: label.to_vec(),
                owner: from,
                expires: later(now, kept.min(UNRESOLVED_KEPT)),
                kept,
                srv: None,
                addresses: Vec::new(),
            };
            self.instances.insert(key, fresh);
            changed = true;
        }
        for r in &m.records {
            let (TYPE_SRV, Data::Srv { port, target }) = (r.rtype, &r.data) else { continue };
            let Some(label) = r.name.child_of(&self.service) else { continue };
            let Some(i) = self.instances.get_mut(&label.to_ascii_lowercase()) else { continue };
            if i.owner != from {
                continue;
            }
            if r.ttl == 0 {
                if let Some(srv) = i.srv.as_mut() {
                    srv.2 = later(now, GOODBYE_GRACE).min(srv.2);
                }
                self.reconfirm = true;
                continue;
            }
            if i.srv.as_ref().is_none_or(|(t, p, _)| !t.same(target) || p != port) {
                // Moved: its old addresses were for the old host.
                i.addresses.clear();
                changed = true;
            }
            i.srv = Some((target.clone(), *port, later(now, kept_for(r.ttl))));
        }
        for r in &m.records {
            let ip = match r.data {
                Data::A(a) => IpAddr::V4(a),
                Data::Aaaa(a) => IpAddr::V6(a),
                _ => continue,
            };
            // Addresses only from the computer the server belongs to, for the host it points to.
            for i in self.instances.values_mut().filter(|i| i.owner == from) {
                let Some((target, _, _)) = &i.srv else { continue };
                if !target.same(&r.name) {
                    continue;
                }
                let was_resolved = i.resolved();
                if r.flush && r.ttl != 0 {
                    // Replaces what it said more than a second ago (RFC 6762 §10.2).
                    let before = i.addresses.len();
                    i.addresses.retain(|(_, _, heard)| now.duration_since(*heard) <= Duration::from_secs(1));
                    changed |= i.addresses.len() != before;
                }
                match i.addresses.iter().position(|(a, _, _)| *a == ip) {
                    Some(at) if r.ttl == 0 => {
                        if let Some(entry) = i.addresses.get_mut(at) {
                            entry.1 = later(now, GOODBYE_GRACE).min(entry.1);
                        }
                        self.reconfirm = true;
                    }
                    Some(at) => {
                        if let Some(entry) = i.addresses.get_mut(at) {
                            entry.1 = later(now, kept_for(r.ttl));
                            entry.2 = now;
                        }
                    }
                    None if r.ttl == 0 => {}
                    None if i.addresses.len() < MAX_ADDRESSES => {
                        i.addresses.push((ip, later(now, kept_for(r.ttl)), now));
                        changed = true;
                    }
                    None => {}
                }
                if r.ttl != 0 && i.resolved() && !was_resolved {
                    // Resolved now: kept as long as its PTR said, not just the short wait.
                    i.expires = i.expires.max(later(now, i.kept));
                }
            }
        }
        changed
    }

    /// Forgets what has expired; returns whether anything changed.
    pub fn expire(&mut self, now: Instant) -> bool {
        let shape = |h: &Heard| -> Vec<(Vec<u8>, bool, usize)> {
            h.instances.iter().map(|(k, i)| (k.clone(), i.srv.is_some(), i.addresses.len())).collect()
        };
        let before = shape(self);
        self.instances.retain(|_, i| i.expires > now);
        for i in self.instances.values_mut() {
            if i.srv.as_ref().is_some_and(|(_, _, e)| *e <= now) {
                i.srv = None;
                i.addresses.clear();
            }
            i.addresses.retain(|(_, e, _)| *e > now);
        }
        before != shape(self)
    }

    /// The servers it has all it needs for.
    pub fn resolved(&self) -> Vec<Resolved> {
        self.instances
            .values()
            .filter_map(|i| {
                let (target, port, _) = i.srv.as_ref()?;
                let addresses: Vec<IpAddr> = i.addresses.iter().map(|(a, _, _)| *a).collect();
                (!addresses.is_empty()).then(|| Resolved { instance: i.label.clone(), host: target.text(), port: *port, addresses })
            })
            .collect()
    }

    /// What to ask next: PlanCraft servers, and what is missing for the ones heard of (at most
    /// 16 questions).
    fn questions(&self) -> Vec<Question> {
        let mut qs = vec![Question { name: self.service.clone(), qtype: TYPE_PTR }];
        for i in self.instances.values() {
            match &i.srv {
                None => qs.push(Question { name: Name::child(&i.label, &self.service), qtype: TYPE_SRV }),
                Some((target, _, _)) if i.addresses.is_empty() => qs.push(Question { name: target.clone(), qtype: TYPE_A }),
                Some(_) => {}
            }
        }
        qs.truncate(16);
        qs
    }

    /// Takes the "ask again at once" mark.
    fn take_reconfirm(&mut self) -> bool {
        std::mem::take(&mut self.reconfirm)
    }

    /// Records kept now, all kinds (tests: it never grows past its bounds).
    pub fn size(&self) -> usize {
        self.instances.values().map(|i| 1usize.saturating_add(usize::from(i.srv.is_some())).saturating_add(i.addresses.len())).sum()
    }
}

/// What a listener shares with whoever shows its servers.
#[derive(Default)]
struct Shared {
    /// Bumped whenever `resolved` changes.
    generation: u64,
    resolved: Vec<Resolved>,
    waker: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// Asks for PlanCraft servers and keeps what they say, until dropped.
pub struct Listener {
    stop: Arc<AtomicBool>,
    shared: Arc<Mutex<Shared>>,
    thread: Option<JoinHandle<()>>,
}

fn lock(m: &Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Listener {
    /// Starts listening for `service` on mDNS `port`, on the networks `find_nets` gives (none yet
    /// is fine: they are looked at again every few seconds).
    pub fn start(service: Name, port: u16, find_nets: fn() -> Vec<Net>) -> Result<Listener, String> {
        let mut link = Link::open(port, find_nets)?;
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (stopped, out) = (Arc::clone(&stop), Arc::clone(&shared));
        let thread = std::thread::Builder::new()
            .name("plancraft-mdns-listener".into())
            .spawn(move || listen(&mut link, Heard::new(service), &stopped, &out))
            .map_err(|e| format!("cannot look for servers on the network: {e}"))?;
        Ok(Listener { stop, shared, thread: Some(thread) })
    }

    /// Called whenever the servers heard of change (from the listener's thread), a few times a
    /// second at most.
    pub fn set_waker(&self, waker: Arc<dyn Fn() + Send + Sync>) {
        lock(&self.shared).waker = Some(waker);
    }

    /// A number that changes whenever the servers heard of do.
    pub fn generation(&self) -> u64 {
        lock(&self.shared).generation
    }

    /// The servers heard of, and a number that changes whenever they do.
    pub fn resolved(&self) -> (u64, Vec<Resolved>) {
        let s = lock(&self.shared);
        (s.generation, s.resolved.clone())
    }
}

impl Drop for Listener {
    /// Stops listening; the thread ends on its own within a moment (never waited for here).
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        drop(self.thread.take());
    }
}

fn listen(link: &mut Link, mut heard: Heard, stop: &AtomicBool, out: &Mutex<Shared>) {
    // Asked at once, then after 1, 2, 4… seconds, [`MAX_QUERY_GAP`] apart at most (RFC 6762
    // §5.2), so servers are heard again well within their records' lifetimes.
    let mut next_query = Instant::now();
    let mut gap = Duration::from_secs(1);
    let mut last_expiry = Instant::now();
    let mut last_wake: Option<Instant> = None;
    let mut unpublished = false;
    let mut buf = vec![0u8; MAX_PACKET];
    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        if heard.take_reconfirm() {
            next_query = now;
            gap = Duration::from_secs(1);
        }
        if now >= next_query {
            let packet = query(&heard.questions());
            for net in &link.nets {
                link.send_multicast(net, &packet);
            }
            next_query = later(now, gap);
            gap = gap.saturating_mul(2).min(MAX_QUERY_GAP);
        }
        if now.duration_since(link.scanned) >= RESCAN && !link.rescan().is_empty() {
            // A network joined: ask there soon.
            next_query = Instant::now();
            gap = Duration::from_secs(1);
        }
        if now.duration_since(last_expiry) >= Duration::from_millis(500) {
            unpublished |= heard.expire(now);
            last_expiry = now;
        }
        if let Some((n, from, _)) = link.receive(&mut buf)
            && from.port() == link.port
            && let Some(m) = buf.get(..n).and_then(|p| parse_part(p, Part::Records))
        {
            unpublished |= heard.take(&m, *from.ip(), Instant::now());
        }
        // Told about at most every quarter second, however much changes.
        if unpublished && last_wake.is_none_or(|t| t.elapsed() >= WAKE_GAP) {
            unpublished = false;
            last_wake = Some(Instant::now());
            let waker = {
                let mut s = lock(out);
                s.resolved = heard.resolved();
                s.generation = s.generation.wrapping_add(1);
                s.waker.clone()
            };
            if let Some(w) = waker {
                w();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> Service {
        Service {
            service: Name::dotted("_plancraft._tcp.local."),
            instance: b"Plans on studio.pc".to_vec(),
            host: Name::dotted("studio-pc-plancraft.local."),
            port: 47400,
            txt: vec!["v=3".into()],
            only: None,
        }
    }

    const NET: Net = Net { ip: Ipv4Addr::new(192, 168, 1, 20), mask: Ipv4Addr::new(255, 255, 255, 0), iface: None };
    const SERVER: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 20);

    fn heard_from(heard: &mut Heard, s: &Service, from: Ipv4Addr, net_ip: Ipv4Addr, how: Way, now: Instant) -> bool {
        let net = Net { ip: net_ip, ..NET };
        heard.take(&parse(&s.records(&net, how)).unwrap(), from, now)
    }

    #[test]
    fn an_announcement_reads_back_and_resolves() {
        let s = service();
        let m = parse(&s.records(&NET, Way::Multicast)).unwrap();
        assert!(m.response);
        assert_eq!(m.records.len(), 4);
        assert!(m.records.iter().filter(|r| r.rtype != TYPE_PTR).all(|r| r.flush));
        let mut heard = Heard::new(s.service.clone());
        let now = Instant::now();
        assert!(heard.take(&m, SERVER, now));
        assert_eq!(
            heard.resolved(),
            [Resolved {
                instance: b"Plans on studio.pc".to_vec(),
                host: "studio-pc-plancraft.local.".into(),
                port: 47400,
                addresses: vec![IpAddr::V4(NET.ip)],
            }]
        );
        // Kept while heard again, for as long as it said (capped), not just the short wait.
        assert!(!heard.expire(now + UNRESOLVED_KEPT + Duration::from_secs(1)));
        // Goodbye: gone a second later (unless it answers meanwhile), and asked about again.
        heard_from(&mut heard, &s, SERVER, SERVER, Way::Goodbye, now);
        assert!(heard.take_reconfirm());
        assert_eq!(heard.resolved().len(), 1);
        assert!(heard.expire(now + Duration::from_secs(2)));
        assert!(heard.resolved().is_empty());
    }

    /// Regression: the goodbye withdrew the host's address, which every server on that computer
    /// shares: stopping one hid the others.
    #[test]
    fn a_goodbye_leaves_the_other_servers_on_that_computer() {
        let a = service();
        let b = Service { instance: b"Other on studio.pc".to_vec(), port: 47401, ..service() };
        let goodbye = parse(&a.records(&NET, Way::Goodbye)).unwrap();
        assert!(!goodbye.records.iter().any(|r| r.rtype == TYPE_A), "{goodbye:?}");
        let mut heard = Heard::new(a.service.clone());
        let now = Instant::now();
        heard_from(&mut heard, &a, SERVER, SERVER, Way::Multicast, now);
        heard_from(&mut heard, &b, SERVER, SERVER, Way::Multicast, now);
        heard.take(&goodbye, SERVER, now);
        heard.expire(now + Duration::from_secs(2));
        let r = heard.resolved();
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].port, r[0].addresses.clone()), (47401, vec![IpAddr::V4(SERVER)]));
    }

    /// Regression: one computer could fill the listener (256 servers in one packet), take over a
    /// server's address list, point another's server elsewhere or say goodbye for it.
    #[test]
    fn a_server_belongs_to_the_computer_that_announced_it() {
        let svc = Name::dotted("_plancraft._tcp.local.");
        let evil = Ipv4Addr::new(192, 168, 1, 66);
        let now = Instant::now();
        let mut heard = Heard::new(svc.clone());
        // A flood of servers from one computer: 8 kept.
        let records: Vec<Record> = (0..256)
            .map(|i| Record {
                name: svc.clone(),
                rtype: TYPE_PTR,
                ttl: u32::MAX,
                flush: false,
                data: Data::Ptr(Name::child(format!("Flood {i}").as_bytes(), &svc)),
            })
            .collect();
        heard.take(&Message { response: true, records, ..Message::default() }, evil, now);
        assert_eq!(heard.instances.len(), MAX_PER_SENDER);
        // The real server, heard afterwards, gets in and resolves.
        let s = service();
        heard_from(&mut heard, &s, SERVER, SERVER, Way::Multicast, now);
        assert_eq!(heard.resolved().len(), 1);
        // Someone else can't point it elsewhere, add addresses or say goodbye for it.
        let hijack = Service { host: Name::dotted("evil.local."), port: 1, ..service() };
        heard_from(&mut heard, &hijack, evil, evil, Way::Multicast, now);
        heard_from(&mut heard, &s, evil, evil, Way::Multicast, now);
        heard_from(&mut heard, &s, evil, evil, Way::Goodbye, now);
        heard.expire(now + Duration::from_secs(5));
        let r = heard.resolved();
        assert_eq!(r.len(), 1);
        assert_eq!(
            (r[0].host.as_str(), r[0].port, r[0].addresses.clone()),
            ("studio-pc-plancraft.local.", 47400, vec![IpAddr::V4(SERVER)])
        );
        // Servers never told where they are go soon, making room.
        assert!(heard.expire(now + UNRESOLVED_KEPT + Duration::from_secs(1)));
        assert_eq!(heard.instances.len(), 1);
    }

    #[test]
    fn a_new_address_replaces_the_old() {
        let s = service();
        let mut heard = Heard::new(s.service.clone());
        let now = Instant::now();
        heard_from(&mut heard, &s, SERVER, SERVER, Way::Multicast, now);
        // Its address changed (cache-flush): the old one goes.
        heard_from(&mut heard, &s, SERVER, Ipv4Addr::new(192, 168, 1, 21), Way::Multicast, now + Duration::from_secs(5));
        assert_eq!(heard.resolved()[0].addresses, [IpAddr::V4(Ipv4Addr::new(192, 168, 1, 21))]);
    }

    #[test]
    fn questions_about_the_server_are_recognised() {
        let s = service();
        let ask = |name: &str, qtype| s.asked(&Question { name: Name::dotted(name), qtype });
        assert!(ask("_PlanCraft._tcp.local.", TYPE_PTR));
        assert!(ask("studio-pc-plancraft.local.", TYPE_A));
        assert!(s.asked(&Question { name: s.instance_name(), qtype: TYPE_SRV }));
        assert!(!ask("_http._tcp.local.", TYPE_PTR));
        assert!(!ask("_plancraft._tcp.local.", TYPE_A));
        assert!(Service::asked_listing(&Question { name: Name::dotted(SERVICES_META), qtype: TYPE_PTR }));
        let q = parse(&query(&[Question { name: s.service.clone(), qtype: TYPE_PTR }])).unwrap();
        assert!(!q.response && q.questions.len() == 1 && s.asked(&q.questions[0]));
        // A one-shot querier gets its id and only its questions about us back, short lifetimes,
        // and no cache-flush (RFC 6762 §10.2).
        let mut m = q.clone();
        m.id = 77;
        m.questions.push(Question { name: Name::dotted("other.local."), qtype: TYPE_A });
        let a = parse(&s.unicast_answer(&NET, &m)).unwrap();
        assert_eq!((a.id, a.questions.len(), a.records.len()), (77, 1, 4));
        assert!(a.records.iter().all(|r| r.ttl == 10 && !r.flush), "{:?}", a.records);
        // The listing of service types names ours.
        let l = parse(&s.answer(&NET, true)).unwrap();
        assert!(l.records.iter().any(|r| r.name.same(&Name::dotted(SERVICES_META)) && r.data == Data::Ptr(s.service.clone())));
        // A responder reads only queries, a listener only responses.
        assert!(parse_part(&s.records(&NET, Way::Multicast), Part::Questions).is_none());
        assert!(parse_part(&query(&[]), Part::Records).is_none());
    }

    #[test]
    fn only_neighbours_are_heard() {
        let other = Net::new(Ipv4Addr::new(10, 8, 0, 2), 16, None).unwrap();
        let nets = [NET, other];
        assert_eq!(net_of(&nets, Ipv4Addr::new(192, 168, 1, 77)), Some(NET));
        assert!(net_of(&nets, Ipv4Addr::new(10, 8, 200, 1)).is_some());
        assert_eq!(net_of(&nets, Ipv4Addr::new(192, 168, 2, 1)), None);
        assert_eq!(net_of(&nets, Ipv4Addr::new(8, 8, 8, 8)), None);
        // Network and broadcast addresses, and addresses no one sends from.
        for odd in [Ipv4Addr::new(192, 168, 1, 0), Ipv4Addr::new(192, 168, 1, 255), Ipv4Addr::UNSPECIFIED, Ipv4Addr::BROADCAST, GROUP] {
            assert_eq!(net_of(&nets, odd), None, "{odd}");
        }
        // Netmasks come from the prefix; ones that would let everyone in are refused.
        assert_eq!(Net::new(Ipv4Addr::new(10, 0, 0, 1), 24, None).unwrap().mask, Ipv4Addr::new(255, 255, 255, 0));
        assert_eq!(Net::new(Ipv4Addr::new(10, 0, 0, 1), 0, None), None);
        assert_eq!(Net::new(Ipv4Addr::new(10, 0, 0, 1), 4, None), None);
        let p2p = Net::new(Ipv4Addr::new(10, 0, 0, 1), 32, None).unwrap();
        assert!(p2p.holds(Ipv4Addr::new(10, 0, 0, 1)) && !p2p.holds(Ipv4Addr::new(10, 0, 0, 2)));
    }

    /// Regression: the mDNS library kept every record anyone sent, for as long as they said
    /// (up to 136 years), with no limit: a flood filled memory in seconds.
    #[test]
    fn a_flood_of_records_stays_bounded() {
        let svc = Name::dotted("_plancraft._tcp.local.");
        let mut heard = Heard::new(svc.clone());
        let now = Instant::now();
        // More computers than it keeps servers from, each sending far more than it keeps.
        for i in 0..600u32 {
            let from = Ipv4Addr::from(0x0A00_0000 | i);
            let label = format!("Flood {i}").into_bytes();
            let instance = Name::child(&label, &svc);
            let host = Name::dotted(&format!("h{i}.local."));
            let mut records = vec![
                Record { name: svc.clone(), rtype: TYPE_PTR, ttl: u32::MAX, flush: false, data: Data::Ptr(instance.clone()) },
                Record { name: instance, rtype: TYPE_SRV, ttl: u32::MAX, flush: true, data: Data::Srv { port: 1, target: host.clone() } },
            ];
            for a in 0..100u8 {
                records.push(Record {
                    name: host.clone(),
                    rtype: TYPE_A,
                    ttl: u32::MAX,
                    flush: false,
                    data: Data::A(Ipv4Addr::new(10, 0, (i % 250) as u8, a)),
                });
            }
            heard.take(&Message { id: 0, response: true, questions: Vec::new(), records }, from, now);
        }
        assert!(heard.instances.len() <= MAX_INSTANCES);
        assert!(heard.instances.values().all(|i| i.addresses.len() <= MAX_ADDRESSES));
        assert!(heard.size() <= MAX_INSTANCES * (2 + MAX_ADDRESSES));
        // Lifetimes are capped: everything is gone a few minutes later.
        assert!(heard.expire(now + MAX_KEPT + Duration::from_secs(1)));
        assert_eq!(heard.size(), 0);
    }

    #[test]
    fn bad_names_are_refused() {
        // A pointer to itself, a pointer forwards, a label running off the end, reserved bits.
        let header = [0u8, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 0];
        for name in [&[0xC0u8, 12][..], &[0xC0, 40], &[5, b'a'], &[0x40, 1, 0], &[0x80, 1, 0]] {
            let mut p = header.to_vec();
            p.extend_from_slice(name);
            p.extend_from_slice(&[0, 1, 0, 1, 0, 0, 0, 1, 0, 4, 1, 2, 3, 4]);
            assert_eq!(parse(&p), None, "{name:?}");
        }
        // Names over 255 bytes (the final zero counted) are refused.
        let mut p = header.to_vec();
        for _ in 0..4 {
            p.push(63);
            p.extend_from_slice(&[b'x'; 63]);
        }
        p.push(0);
        p.extend_from_slice(&[0, 1, 0, 1, 0, 0, 0, 1, 0, 4, 1, 2, 3, 4]);
        assert_eq!(parse(&p), None);
    }

    /// Regression: a packet of long names pointing at each other cost milliseconds to read (a
    /// few hundred a second kept a thread busy).
    #[test]
    fn a_packet_of_long_names_is_cheap() {
        // One name of 127 one-letter labels, then 254 more records naming it by pointer.
        let mut p = vec![0u8, 0, 0x84, 0, 0, 0, 0, 255, 0, 0, 0, 0];
        for _ in 0..127 {
            p.extend_from_slice(&[1, b'x']);
        }
        p.push(0);
        p.extend_from_slice(&[0, 1, 0, 1, 0, 0, 0, 1, 0, 4, 1, 2, 3, 4]);
        for _ in 1..255 {
            p.extend_from_slice(&[0xC0, 12, 0, 1, 0, 1, 0, 0, 0, 1, 0, 4, 1, 2, 3, 4]);
        }
        let start = Instant::now();
        for _ in 0..100 {
            // Stops once the packet's budget for names is spent.
            assert_eq!(parse(&p), None);
        }
        assert!(start.elapsed() < Duration::from_millis(500), "{:?}", start.elapsed());
    }

    #[test]
    fn compressed_names_read() {
        let mut p = vec![0u8, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 0];
        // Record name: "a.local." written in full.
        p.extend_from_slice(&[1, b'a', 5, b'l', b'o', b'c', b'a', b'l', 0]);
        p.extend_from_slice(&[0, 12, 0, 1, 0, 0, 0, 120]);
        // PTR to "b" + pointer to "local." (offset 14).
        p.extend_from_slice(&[0, 4, 1, b'b', 0xC0, 14]);
        let m = parse(&p).unwrap();
        assert_eq!(m.records[0].data, Data::Ptr(Name::dotted("b.local.")));
    }
}
