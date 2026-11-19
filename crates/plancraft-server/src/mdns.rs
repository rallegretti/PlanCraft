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
//! - **Listener** ([`Listener`]): asks for PlanCraft servers and keeps only their records, in
//!   bounded maps ([`MAX_INSTANCES`], [`MAX_HOSTS`], [`MAX_ADDRESSES`]) with lifetimes capped at
//!   [`MAX_TTL`].
//! - Both hear only computers on a network this one is on: a packet whose sender is outside every
//!   local network (an IPv4 address and its netmask) is dropped unread (RFC 6762 §11).
//! - Packets are parsed with every length, count, label and compression pointer checked: nothing a
//!   packet says can make it panic, loop or allocate much (`tests/sharing.rs` sweeps random
//!   packets).
//!
//! IPv4 only (224.0.0.251): every office network PlanCraft is used on has it; servers announce
//! their IPv4 addresses.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The mDNS multicast group.
pub const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
/// Largest packet read or sent (RFC 6762 §17: 9000 bytes).
const MAX_PACKET: usize = 9000;
/// Questions and records read from one packet, at most (the rest is ignored).
const MAX_QUESTIONS: usize = 64;
const MAX_RECORDS: usize = 256;
/// Compression pointers followed in one name, at most.
const MAX_JUMPS: usize = 64;
/// PlanCraft servers kept by a listener, at most.
pub const MAX_INSTANCES: usize = 256;
/// Host names kept by a listener (those servers point to), at most.
pub const MAX_HOSTS: usize = 256;
/// Addresses kept per host name, at most.
pub const MAX_ADDRESSES: usize = 32;
/// Longest a listener keeps a record, whatever lifetime it was sent with (75 minutes, the
/// lifetime RFC 6762 §10 gives records that are not host names).
pub const MAX_TTL: Duration = Duration::from_secs(4500);
/// Lifetimes of the records a responder sends (RFC 6762 §10).
const HOST_TTL: u32 = 120;
const OTHER_TTL: u32 = 4500;
/// How often the computer's networks are looked at again.
const RESCAN: Duration = Duration::from_secs(5);
/// How long a socket read waits, so stopping and timers are seen.
const TICK: Duration = Duration::from_millis(200);
/// A responder answers on one network at most this often (RFC 6762 §6).
const ANSWER_GAP: Duration = Duration::from_secs(1);
/// Unicast answers (to one-shot queriers) per second, at most.
const MAX_UNICAST_PER_SECOND: u32 = 20;

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

    /// As text, lower case, dots between labels and a dot at the end, dots and backslashes in a
    /// label escaped (`studio-pc.local.`, `Plans\.2 on pc._plancraft._tcp.local.`).
    pub fn text(&self) -> String {
        let mut out = String::new();
        for label in &self.0 {
            out.push_str(&escape_label(label));
            out.push('.');
        }
        out.to_lowercase()
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

    fn bytes(&mut self, n: usize) -> Option<&[u8]> {
        let end = self.pos.checked_add(n)?;
        let b = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(b)
    }

    /// A name, following compression pointers (only backwards, at most [`MAX_JUMPS`]; at most
    /// 255 bytes in all, labels of at most 63).
    fn name(&mut self) -> Option<Name> {
        let mut labels = Vec::new();
        let mut total = 0usize;
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
                    at = target;
                }
                _ => return None,
            }
        }
    }
}

/// Reads a packet. `None` when it is not one (cut short, a bad name, a pointer going forwards):
/// nothing a packet holds can make this panic or take long.
pub fn parse(buf: &[u8]) -> Option<Message> {
    let mut r = Reader { buf, pos: 0 };
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
    let mut m = Message { id, response: flags & 0x8000 != 0, ..Message::default() };
    for _ in 0..qd.min(MAX_QUESTIONS) {
        let name = r.name()?;
        let qtype = r.u16()?;
        let _class = r.u16()? & !UNICAST_RESPONSE;
        m.questions.push(Question { name, qtype });
    }
    if qd > MAX_QUESTIONS {
        return Some(m);
    }
    let records = an.saturating_add(ns).saturating_add(ar);
    for _ in 0..records.min(MAX_RECORDS) {
        let name = r.name()?;
        let rtype = r.u16()?;
        let class = r.u16()? & !CACHE_FLUSH;
        let ttl = r.u32()?;
        let len = usize::from(r.u16()?);
        let start = r.pos;
        let rdata = r.bytes(len)?;
        let data = match (rtype, class) {
            (TYPE_A, CLASS_IN) => match rdata {
                [a, b, c, d] => Data::A(Ipv4Addr::new(*a, *b, *c, *d)),
                _ => Data::Other,
            },
            (TYPE_AAAA, CLASS_IN) => <[u8; 16]>::try_from(rdata).map(|o| Data::Aaaa(o.into())).unwrap_or(Data::Other),
            (TYPE_PTR, CLASS_IN) => {
                let mut sub = Reader { buf, pos: start };
                match sub.name() {
                    Some(n) if sub.pos <= start.saturating_add(len) => Data::Ptr(n),
                    _ => Data::Other,
                }
            }
            (TYPE_SRV, CLASS_IN) if len >= 7 => {
                let mut sub = Reader { buf, pos: start };
                let fields = (sub.u16(), sub.u16(), sub.u16());
                match (fields, sub.name()) {
                    ((Some(_), Some(_), Some(port)), Some(target)) if sub.pos <= start.saturating_add(len) => Data::Srv { port, target },
                    _ => Data::Other,
                }
            }
            _ => Data::Other,
        };
        m.records.push(Record { name, rtype, ttl, data });
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

/// A network this computer is on: its IPv4 address there and the netmask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Net {
    pub ip: Ipv4Addr,
    pub mask: Ipv4Addr,
}

impl Net {
    /// Whether `ip` is on this network.
    pub fn holds(&self, ip: Ipv4Addr) -> bool {
        let mask = u32::from(self.mask);
        u32::from(ip) & mask == u32::from(self.ip) & mask
    }
}

/// The network a packet from `from` came over, if it is on one of `nets` (else it is dropped).
pub fn net_of(nets: &[Net], from: Ipv4Addr) -> Option<Net> {
    nets.iter().copied().find(|n| n.holds(from))
}

/// Opens the mDNS socket on `port`, sharing it with the system's own responder.
fn open(port: u16) -> std::io::Result<(UdpSocket, socket2::Socket)> {
    let s = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, Some(socket2::Protocol::UDP))?;
    s.set_reuse_address(true)?;
    #[cfg(unix)]
    {
        // Not everywhere (the system's responder may not use it); reusing the address is enough.
        let _ = s.set_reuse_port(true);
    }
    s.bind(&SocketAddr::from(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port)).into())?;
    s.set_multicast_ttl_v4(255)?;
    s.set_multicast_loop_v4(true)?;
    let options = s.try_clone()?;
    let udp: UdpSocket = s.into();
    udp.set_read_timeout(Some(TICK))?;
    Ok((udp, options))
}

/// The socket and the networks it listens on.
struct Link {
    udp: UdpSocket,
    options: socket2::Socket,
    port: u16,
    nets: Vec<Net>,
    find_nets: fn() -> Vec<Net>,
    scanned: Instant,
}

impl Link {
    fn open(port: u16, find_nets: fn() -> Vec<Net>) -> Result<Link, String> {
        let (udp, options) = open(port).map_err(|e| format!("cannot use the network for finding servers: {e}"))?;
        let mut link = Link { udp, options, port, nets: Vec::new(), find_nets, scanned: Instant::now() };
        link.rescan();
        if link.nets.is_empty() {
            return Err("this computer is on no network".into());
        }
        Ok(link)
    }

    /// Looks at the computer's networks again; returns those it joined now.
    fn rescan(&mut self) -> Vec<Net> {
        self.scanned = Instant::now();
        let now = (self.find_nets)();
        for gone in self.nets.iter().filter(|n| !now.contains(n)) {
            let _ = self.udp.leave_multicast_v4(&GROUP, &gone.ip);
        }
        self.nets.retain(|n| now.contains(n));
        let mut joined = Vec::new();
        for net in now {
            if !self.nets.contains(&net) && self.udp.join_multicast_v4(&GROUP, &net.ip).is_ok() {
                self.nets.push(net);
                joined.push(net);
            }
        }
        joined
    }

    fn send_multicast(&self, net: &Net, packet: &[u8]) {
        if self.options.set_multicast_if_v4(&net.ip).is_ok() {
            let _ = self.udp.send_to(packet, SocketAddrV4::new(GROUP, self.port));
        }
    }

    /// The next packet from a computer on one of the networks (with that network), if one came
    /// within a moment.
    fn receive(&self, buf: &mut [u8]) -> Option<(usize, SocketAddrV4, Net)> {
        let (n, from) = self.udp.recv_from(buf).ok()?;
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

impl Service {
    fn instance_name(&self) -> Name {
        Name::child(&self.instance, &self.service)
    }

    /// Its records on `net`, `ttl` 0 for goodbye: PTR, SRV, TXT and A.
    fn records(&self, net: &Net, goodbye: bool) -> Vec<u8> {
        let ttl = |t: u32| if goodbye { 0 } else { t };
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
        let mut w = Writer::new(0, 0x8400, 0, 4);
        w.record(&self.service, TYPE_PTR, false, ttl(OTHER_TTL), &name_bytes(&instance));
        w.record(&instance, TYPE_SRV, true, ttl(HOST_TTL), &srv);
        w.record(&instance, TYPE_TXT, true, ttl(OTHER_TTL), &txt);
        w.record(&self.host, TYPE_A, true, ttl(HOST_TTL), &net.ip.octets());
        w.buf
    }

    /// Whether `q` asks about this server.
    fn asked(&self, q: &Question) -> bool {
        let any = q.qtype == TYPE_ANY;
        (q.name.same(&self.service) && (any || q.qtype == TYPE_PTR))
            || (q.name.same(&self.instance_name()) && (any || q.qtype == TYPE_SRV || q.qtype == TYPE_TXT))
            || (q.name.same(&self.host) && (any || q.qtype == TYPE_A))
    }

    /// An answer straight to a one-shot querier (RFC 6762 §6.7): its id and questions back,
    /// short lifetimes.
    fn unicast_answer(&self, net: &Net, query: &Message) -> Vec<u8> {
        let mut out = Writer::new(query.id, 0x8400, query.questions.len().min(4), 4);
        for q in query.questions.iter().take(4) {
            out.question(q);
        }
        // The records as sent by multicast (after their 12-byte header), living 10 seconds.
        let short = self.records_with_ttl(net, 10);
        out.buf.extend_from_slice(short.get(12..).unwrap_or_default());
        out.buf
    }

    fn records_with_ttl(&self, net: &Net, ttl: u32) -> Vec<u8> {
        let mut packet = self.records(net, false);
        // Rewrite every record's lifetime: walk the packet as written (no compression).
        let mut r = Reader { buf: &packet, pos: 12 };
        let mut spots = Vec::new();
        for _ in 0..4 {
            if r.name().is_none() {
                break;
            }
            let ttl_at = r.pos.saturating_add(4);
            spots.push(ttl_at);
            r.pos = ttl_at.saturating_add(4);
            let Some(len) = r.u16() else { break };
            r.pos = r.pos.saturating_add(usize::from(len));
        }
        for at in spots {
            if let Some(slot) = packet.get_mut(at..at.saturating_add(4)) {
                slot.copy_from_slice(&ttl.to_be_bytes());
            }
        }
        packet
    }
}

/// Announces a server and answers for it until dropped (then says goodbye).
pub struct Responder {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Responder {
    /// Starts answering for `service` on mDNS `port`, on the networks `find_nets` gives (looked at
    /// again every few seconds).
    pub fn start(service: Service, port: u16, find_nets: fn() -> Vec<Net>) -> Result<Responder, String> {
        let mut link = Link::open(port, find_nets)?;
        if let Some(ip) = service.only {
            link.nets.retain(|n| n.ip == ip);
            if link.nets.is_empty() {
                return Err(format!("{ip} is on none of this computer's networks"));
            }
        }
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

fn respond(link: &mut Link, service: &Service, stop: &AtomicBool) {
    // Announced three times, a second apart (RFC 6762 §8.3), on each network as it is joined.
    let mut announce: Vec<(Net, Instant, u8)> = link.nets.iter().map(|n| (*n, Instant::now(), 0)).collect();
    let mut answered: BTreeMap<u32, Instant> = BTreeMap::new();
    let mut unicast_budget = (Instant::now(), 0u32);
    let mut buf = vec![0u8; MAX_PACKET];
    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        for (net, next, sent) in &mut announce {
            if *sent < 3 && now >= *next {
                link.send_multicast(net, &service.records(net, false));
                *sent = sent.saturating_add(1);
                *next = now + Duration::from_secs(1);
            }
        }
        announce.retain(|(_, _, sent)| *sent < 3);
        if now.duration_since(link.scanned) >= RESCAN && service.only.is_none() {
            for net in link.rescan() {
                announce.push((net, Instant::now(), 0));
            }
            answered.retain(|ip, _| link.nets.iter().any(|n| u32::from(n.ip) == *ip));
        }
        let Some((n, from, net)) = link.receive(&mut buf) else { continue };
        let Some(m) = buf.get(..n).and_then(parse) else { continue };
        if m.response || !m.questions.iter().any(|q| service.asked(q)) {
            continue;
        }
        if from.port() != link.port {
            // A one-shot querier (RFC 6762 §6.7): answered directly, a few a second at most.
            if unicast_budget.0.elapsed() >= Duration::from_secs(1) {
                unicast_budget = (Instant::now(), 0);
            }
            if unicast_budget.1 < MAX_UNICAST_PER_SECOND {
                unicast_budget.1 = unicast_budget.1.saturating_add(1);
                let _ = link.udp.send_to(&service.unicast_answer(&net, &m), from);
            }
            continue;
        }
        let last = answered.get(&u32::from(net.ip));
        if last.is_none_or(|t| t.elapsed() >= ANSWER_GAP) {
            link.send_multicast(&net, &service.records(&net, false));
            answered.insert(u32::from(net.ip), Instant::now());
        }
    }
    for net in &link.nets {
        link.send_multicast(net, &service.records(net, true));
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
    expires: Instant,
    srv: Option<(Name, u16, Instant)>,
}

/// What a listener heard, bounded.
#[derive(Debug, Default)]
pub struct Heard {
    service: Name,
    /// By instance label, lower case.
    instances: BTreeMap<Vec<u8>, Instance>,
    /// By host name text, lower case.
    hosts: BTreeMap<String, Vec<(IpAddr, Instant)>>,
}

fn expiry(now: Instant, ttl: u32) -> Instant {
    now + Duration::from_secs(u64::from(ttl)).min(MAX_TTL)
}

impl Heard {
    pub fn new(service: Name) -> Heard {
        Heard { service, ..Heard::default() }
    }

    /// Takes in a response's records about PlanCraft servers (others are not kept); returns
    /// whether anything changed. Lifetime 0 is a goodbye.
    pub fn take(&mut self, m: &Message, now: Instant) -> bool {
        let mut changed = false;
        // Servers first, then where they point, then their addresses (any order in the packet).
        for r in &m.records {
            if let (TYPE_PTR, Data::Ptr(target)) = (r.rtype, &r.data)
                && r.name.same(&self.service)
                && let Some(label) = target.child_of(&self.service)
            {
                let key = label.to_ascii_lowercase();
                if r.ttl == 0 {
                    changed |= self.instances.remove(&key).is_some();
                } else if let Some(i) = self.instances.get_mut(&key) {
                    i.expires = expiry(now, r.ttl);
                } else if self.instances.len() < MAX_INSTANCES {
                    self.instances.insert(key, Instance { label: label.to_vec(), expires: expiry(now, r.ttl), srv: None });
                    changed = true;
                }
            }
        }
        for r in &m.records {
            if let (TYPE_SRV, Data::Srv { port, target }) = (r.rtype, &r.data)
                && let Some(label) = r.name.child_of(&self.service)
                && let Some(i) = self.instances.get_mut(&label.to_ascii_lowercase())
            {
                let srv = (r.ttl != 0).then(|| (target.clone(), *port, expiry(now, r.ttl)));
                let same = i.srv.as_ref().map(|(t, p, _)| (t, p)) == srv.as_ref().map(|(t, p, _)| (t, p));
                changed |= !same;
                i.srv = srv;
            }
        }
        let wanted: Vec<String> = self.instances.values().filter_map(|i| i.srv.as_ref().map(|(t, _, _)| t.text())).collect();
        for r in &m.records {
            let ip = match r.data {
                Data::A(a) => IpAddr::V4(a),
                Data::Aaaa(a) => IpAddr::V6(a),
                _ => continue,
            };
            let host = r.name.text();
            if !wanted.contains(&host) {
                continue;
            }
            if !self.hosts.contains_key(&host) && self.hosts.len() >= MAX_HOSTS {
                continue;
            }
            let list = self.hosts.entry(host).or_default();
            let known = list.iter().position(|(a, _)| *a == ip);
            match known {
                Some(at) if r.ttl == 0 => {
                    list.remove(at);
                    changed = true;
                }
                Some(at) => {
                    if let Some(entry) = list.get_mut(at) {
                        entry.1 = expiry(now, r.ttl);
                    }
                }
                None if r.ttl == 0 => {}
                None if list.len() < MAX_ADDRESSES => {
                    list.push((ip, expiry(now, r.ttl)));
                    changed = true;
                }
                None => {}
            }
        }
        changed
    }

    /// Forgets what has expired, and host names no server points to; returns whether anything
    /// changed.
    pub fn expire(&mut self, now: Instant) -> bool {
        let before = (self.instances.len(), self.hosts.values().map(Vec::len).sum::<usize>());
        let mut srv_gone = false;
        self.instances.retain(|_, i| i.expires > now);
        for i in self.instances.values_mut() {
            if i.srv.as_ref().is_some_and(|(_, _, e)| *e <= now) {
                i.srv = None;
                srv_gone = true;
            }
        }
        let wanted: Vec<String> = self.instances.values().filter_map(|i| i.srv.as_ref().map(|(t, _, _)| t.text())).collect();
        self.hosts.retain(|h, _| wanted.contains(h));
        for list in self.hosts.values_mut() {
            list.retain(|(_, e)| *e > now);
        }
        self.hosts.retain(|_, l| !l.is_empty());
        srv_gone || before != (self.instances.len(), self.hosts.values().map(Vec::len).sum::<usize>())
    }

    /// The servers it has all it needs for.
    pub fn resolved(&self) -> Vec<Resolved> {
        self.instances
            .values()
            .filter_map(|i| {
                let (target, port, _) = i.srv.as_ref()?;
                let host = target.text();
                let addresses: Vec<IpAddr> = self.hosts.get(&host)?.iter().map(|(a, _)| *a).collect();
                Some(Resolved { instance: i.label.clone(), host, port: *port, addresses })
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
                Some((target, _, _)) if !self.hosts.contains_key(&target.text()) => {
                    qs.push(Question { name: target.clone(), qtype: TYPE_A })
                }
                Some(_) => {}
            }
        }
        qs.truncate(16);
        qs
    }

    /// Records kept now, all kinds (tests: it never grows past its bounds).
    pub fn size(&self) -> usize {
        self.instances.len().saturating_add(self.hosts.values().map(Vec::len).sum::<usize>())
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
    /// Starts listening for `service` on mDNS `port`, on the networks `find_nets` gives.
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

    /// Called whenever the servers heard of change (from the listener's thread).
    pub fn set_waker(&self, waker: Arc<dyn Fn() + Send + Sync>) {
        lock(&self.shared).waker = Some(waker);
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
    // Asked at once, then after 1, 2, 4… seconds, a minute apart at most (RFC 6762 §5.2).
    let mut next_query = Instant::now();
    let mut gap = Duration::from_secs(1);
    let mut last_expiry = Instant::now();
    let mut buf = vec![0u8; MAX_PACKET];
    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        if now >= next_query {
            let packet = query(&heard.questions());
            for net in &link.nets {
                link.send_multicast(net, &packet);
            }
            next_query = now + gap;
            gap = (gap * 2).min(Duration::from_secs(60));
        }
        if now.duration_since(link.scanned) >= RESCAN && !link.rescan().is_empty() {
            // A network joined: ask there soon.
            next_query = Instant::now();
            gap = Duration::from_secs(1);
        }
        let mut changed = false;
        if now.duration_since(last_expiry) >= Duration::from_secs(1) {
            changed |= heard.expire(now);
            last_expiry = now;
        }
        if let Some((n, _, _)) = link.receive(&mut buf)
            && let Some(m) = buf.get(..n).and_then(parse)
            && m.response
        {
            changed |= heard.take(&m, Instant::now());
        }
        if changed {
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

    const NET: Net = Net { ip: Ipv4Addr::new(192, 168, 1, 20), mask: Ipv4Addr::new(255, 255, 255, 0) };

    #[test]
    fn an_announcement_reads_back_and_resolves() {
        let s = service();
        let m = parse(&s.records(&NET, false)).unwrap();
        assert!(m.response);
        assert_eq!(m.records.len(), 4);
        let mut heard = Heard::new(s.service.clone());
        assert!(heard.take(&m, Instant::now()));
        let r = heard.resolved();
        assert_eq!(
            r,
            [Resolved {
                instance: b"Plans on studio.pc".to_vec(),
                host: "studio-pc-plancraft.local.".into(),
                port: 47400,
                addresses: vec![IpAddr::V4(NET.ip)],
            }]
        );
        // Goodbye: gone at once.
        let bye = parse(&s.records(&NET, true)).unwrap();
        assert!(heard.take(&bye, Instant::now()));
        assert!(heard.resolved().is_empty());
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
        let q = parse(&query(&[Question { name: s.service.clone(), qtype: TYPE_PTR }])).unwrap();
        assert!(!q.response && q.questions.len() == 1 && s.asked(&q.questions[0]));
        // A one-shot querier gets its id and question back, and short lifetimes.
        let mut m = q.clone();
        m.id = 77;
        let a = parse(&s.unicast_answer(&NET, &m)).unwrap();
        assert_eq!((a.id, a.questions.len(), a.records.len()), (77, 1, 4));
        assert!(a.records.iter().all(|r| r.ttl == 10), "{:?}", a.records);
    }

    #[test]
    fn only_neighbours_are_heard() {
        let nets = [NET, Net { ip: Ipv4Addr::new(10, 8, 0, 2), mask: Ipv4Addr::new(255, 255, 0, 0) }];
        assert_eq!(net_of(&nets, Ipv4Addr::new(192, 168, 1, 77)), Some(NET));
        assert!(net_of(&nets, Ipv4Addr::new(10, 8, 200, 1)).is_some());
        assert_eq!(net_of(&nets, Ipv4Addr::new(192, 168, 2, 1)), None);
        assert_eq!(net_of(&nets, Ipv4Addr::new(8, 8, 8, 8)), None);
    }

    /// Regression: the mDNS library kept every record anyone sent, for as long as they said
    /// (up to 136 years), with no limit: a flood filled memory in seconds.
    #[test]
    fn a_flood_of_records_stays_bounded() {
        let svc = Name::dotted("_plancraft._tcp.local.");
        let mut heard = Heard::new(svc.clone());
        let now = Instant::now();
        for i in 0..5_000u32 {
            let label = format!("Flood {i}").into_bytes();
            let instance = Name::child(&label, &svc);
            let host = Name::dotted(&format!("h{i}.local."));
            let mut records = vec![
                Record { name: svc.clone(), rtype: TYPE_PTR, ttl: u32::MAX, data: Data::Ptr(instance.clone()) },
                Record { name: instance, rtype: TYPE_SRV, ttl: u32::MAX, data: Data::Srv { port: 1, target: host.clone() } },
            ];
            for a in 0..100u8 {
                records.push(Record {
                    name: host.clone(),
                    rtype: TYPE_A,
                    ttl: u32::MAX,
                    data: Data::A(Ipv4Addr::new(10, 0, (i % 250) as u8, a)),
                });
            }
            // Records about other names are never kept.
            records.push(Record {
                name: Name::dotted(&format!("x{i}.local.")),
                rtype: TYPE_A,
                ttl: u32::MAX,
                data: Data::A(Ipv4Addr::LOCALHOST),
            });
            heard.take(&Message { id: 0, response: true, questions: Vec::new(), records }, now);
        }
        assert!(heard.instances.len() <= MAX_INSTANCES);
        assert!(heard.hosts.len() <= MAX_HOSTS);
        assert!(heard.hosts.values().all(|l| l.len() <= MAX_ADDRESSES));
        assert!(heard.size() <= MAX_INSTANCES + MAX_HOSTS * MAX_ADDRESSES);
        // Lifetimes are capped: everything is gone after 75 minutes.
        assert!(heard.expire(now + MAX_TTL + Duration::from_secs(1)));
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
        // A long chain of backward pointers is cut off.
        let mut p = header.to_vec();
        p.push(0);
        for i in 0..200u16 {
            let at = 12 + 2 * i;
            p.extend_from_slice(&(0xC000 | at.saturating_sub(1).max(12)).to_be_bytes());
        }
        let _ = parse(&p);
        // Names over 255 bytes are refused.
        let mut p = header.to_vec();
        for _ in 0..5 {
            p.push(63);
            p.extend_from_slice(&[b'x'; 63]);
        }
        p.push(0);
        p.extend_from_slice(&[0, 1, 0, 1, 0, 0, 0, 1, 0, 4, 1, 2, 3, 4]);
        assert_eq!(parse(&p), None);
    }

    #[test]
    fn compressed_names_read() {
        // "local." at 12, then "_tcp" + pointer to it.
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
