//! The encrypted connection between PlanCraft and PlanCraft Server.
//!
//! It uses the Noise protocol (`Noise_NX_25519_ChaChaPoly_BLAKE2s`, through the pure-Rust `snow`
//! crate), not TLS: TLS in Rust needs `ring` or `aws-lc-rs`, which compile C. The server has a
//! long-term key pair; a client learns the server's public key during the handshake and checks
//! it against the fingerprint it pinned on its first visit, as SSH does. Passwords are only ever
//! sent inside the encrypted connection.
//!
//! On the network every Noise message is a two-byte length and the message. A frame (one JSON
//! message) is a four-byte length and its bytes, split over as many Noise messages as it needs.
//! Sizes are checked before anything is allocated, and a frame is read only up to the size the
//! caller allows.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::Path;
use std::sync::Arc;

use blake2::{Blake2s256, Digest};
use plancraft_core::io::{Privacy, write_atomic};
use serde::{Deserialize, Serialize};
use snow::{Builder, StatelessTransportState};

/// The Noise handshake: the server sends its static key, the client has none (it signs in with
/// a password instead).
pub const PATTERN: &str = "Noise_NX_25519_ChaChaPoly_BLAKE2s";
/// Binds the handshake to this program and protocol.
const PROLOGUE: &[u8] = b"PlanCraft Server protocol 1";
const MAX_NOISE: usize = 65_535;
const TAG: usize = 16;
/// Most plaintext bytes in one Noise message.
const MAX_CHUNK: usize = MAX_NOISE - TAG;
/// Most bytes of a key file read.
const MAX_KEY_FILE: u64 = 4096;

#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("network error: {0}")]
    Io(#[from] std::io::Error),
    #[error("the secure connection failed: {0}")]
    Noise(String),
    #[error("the other side sent a frame of {0} bytes, more than the {1} allowed")]
    TooLarge(usize, usize),
    #[error("the other side sent a damaged frame")]
    Damaged,
    #[error("no room for a frame of {0} bytes now (others are being read)")]
    NoRoom(usize),
    #[error("a frame of {0} bytes arrived too slowly")]
    TooSlow(usize),
    #[error(
        "the server's key is not the one this computer knows (fingerprint {found}); it may be another computer pretending to be the server"
    )]
    WrongServer { found: Fingerprint },
}

impl From<snow::Error> for WireError {
    fn from(e: snow::Error) -> Self {
        WireError::Noise(e.to_string())
    }
}

/// The short form of a server's public key that people compare, e.g. `3f9a 0c41 …` (16 bytes of
/// its BLAKE2s hash, in eight groups).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Fingerprint(pub String);

impl Fingerprint {
    pub fn of(public_key: &[u8]) -> Self {
        let digest = Blake2s256::digest(public_key);
        let hex = to_hex(digest.get(..16).unwrap_or_default());
        let groups: Vec<&str> = (0..8).filter_map(|i| hex.get(i * 4..i * 4 + 4)).collect();
        Fingerprint(groups.join(" "))
    }

    /// Compares ignoring spaces and case (people type fingerprints in).
    pub fn matches(&self, other: &str) -> bool {
        let clean = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
        clean(&self.0) == clean(other)
    }
}

impl std::fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Which server key a client accepts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trust {
    /// Any key: the first visit, when the person is shown the fingerprint to confirm.
    FirstUse,
    /// Only the key with this fingerprint.
    Pinned(Fingerprint),
}

/// The server's long-term key pair.
#[derive(Clone)]
pub struct Keys {
    private: Vec<u8>,
    public: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct KeyFile {
    private: String,
    public: String,
}

impl Keys {
    pub fn generate() -> Result<Keys, WireError> {
        let kp = Builder::new(params()?).generate_keypair()?;
        Ok(Keys { private: kp.private, public: kp.public })
    }

    /// Loads the key pair from `path`, or makes one and saves it there (readable by the owner only).
    pub fn load_or_create(path: &Path) -> Result<Keys, String> {
        if path.exists() {
            let meta = std::fs::symlink_metadata(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            if !meta.is_file() {
                return Err(format!("{} is not a regular file", path.display()));
            }
            crate::users::keep_private(path, &meta);
            let mut text = String::new();
            std::fs::File::open(path)
                .and_then(|f| f.take(MAX_KEY_FILE).read_to_string(&mut text))
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let file: KeyFile = serde_json::from_str(&text).map_err(|e| format!("{} is damaged: {e}", path.display()))?;
            let keys = Keys {
                private: from_hex(&file.private).ok_or_else(|| format!("{} is damaged", path.display()))?,
                public: from_hex(&file.public).ok_or_else(|| format!("{} is damaged", path.display()))?,
            };
            let private: [u8; 32] = keys.private.as_slice().try_into().map_err(|_| format!("{} is damaged", path.display()))?;
            // The public key must belong to the private one, or clients would pin a key the
            // server cannot prove it holds.
            if curve25519_dalek::MontgomeryPoint::mul_base_clamped(private).to_bytes().as_slice() != keys.public.as_slice() {
                return Err(format!("{} is damaged: its two keys do not belong together", path.display()));
            }
            return Ok(keys);
        }
        let keys = Keys::generate().map_err(|e| e.to_string())?;
        let file = KeyFile { private: to_hex(&keys.private), public: to_hex(&keys.public) };
        let text = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            plancraft_core::io::create_private_dir(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        write_atomic(path, text.as_bytes(), Privacy::OwnerOnly).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        Ok(keys)
    }

    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint::of(&self.public)
    }
}

fn params() -> Result<snow::params::NoiseParams, WireError> {
    PATTERN.parse().map_err(|e: snow::Error| WireError::Noise(e.to_string()))
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len() / 2).map(|i| s.get(i * 2..i * 2 + 2).and_then(|b| u8::from_str_radix(b, 16).ok())).collect()
}

/// The sending half of a connection.
pub struct Sender {
    stream: TcpStream,
    state: Arc<StatelessTransportState>,
    nonce: u64,
}

/// The receiving half of a connection.
pub struct Receiver {
    stream: TcpStream,
    state: Arc<StatelessTransportState>,
    nonce: u64,
}

fn write_noise(stream: &mut TcpStream, msg: &[u8]) -> Result<(), WireError> {
    let len = u16::try_from(msg.len()).map_err(|_| WireError::Damaged)?;
    let mut out = Vec::with_capacity(msg.len() + 2);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(msg);
    stream.write_all(&out)?;
    Ok(())
}

/// The slowest a large frame may arrive at, in bytes a second.
const SLOWEST_LARGE_FRAME: usize = 256 * 1024;

/// How long a frame of `len` bytes may take to arrive once its length is known: a minute, or
/// longer for a frame that needs it at [`SLOWEST_LARGE_FRAME`].
pub fn frame_deadline(len: usize) -> std::time::Duration {
    let secs = u64::try_from(len / SLOWEST_LARGE_FRAME).unwrap_or(u64::MAX);
    std::time::Duration::from_secs(secs.max(60))
}

fn read_noise(stream: &mut TcpStream) -> Result<Vec<u8>, WireError> {
    let mut len = [0u8; 2];
    stream.read_exact(&mut len)?;
    let len = usize::from(u16::from_be_bytes(len));
    let mut msg = vec![0u8; len];
    stream.read_exact(&mut msg)?;
    Ok(msg)
}

fn split(stream: TcpStream, state: StatelessTransportState) -> Result<(Sender, Receiver), WireError> {
    let state = Arc::new(state);
    let other = stream.try_clone()?;
    Ok((Sender { stream, state: Arc::clone(&state), nonce: 0 }, Receiver { stream: other, state, nonce: 0 }))
}

/// The server's side of the handshake.
pub fn accept(mut stream: TcpStream, keys: &Keys) -> Result<(Sender, Receiver), WireError> {
    stream.set_nodelay(true)?;
    let mut hs = Builder::new(params()?).prologue(PROLOGUE)?.local_private_key(&keys.private)?.build_responder()?;
    let mut buf = vec![0u8; MAX_NOISE];
    let first = read_noise(&mut stream)?;
    hs.read_message(&first, &mut buf)?;
    let n = hs.write_message(&[], &mut buf)?;
    write_noise(&mut stream, buf.get(..n).ok_or(WireError::Damaged)?)?;
    split(stream, hs.into_stateless_transport_mode()?)
}

/// The client's side of the handshake. Returns the server's fingerprint (to pin on a first visit).
pub fn connect(mut stream: TcpStream, trust: &Trust) -> Result<(Sender, Receiver, Fingerprint), WireError> {
    stream.set_nodelay(true)?;
    let mut hs = Builder::new(params()?).prologue(PROLOGUE)?.build_initiator()?;
    let mut buf = vec![0u8; MAX_NOISE];
    let n = hs.write_message(&[], &mut buf)?;
    write_noise(&mut stream, buf.get(..n).ok_or(WireError::Damaged)?)?;
    let reply = read_noise(&mut stream)?;
    hs.read_message(&reply, &mut buf)?;
    let found = Fingerprint::of(hs.get_remote_static().ok_or(WireError::Damaged)?);
    if let Trust::Pinned(expected) = trust
        && *expected != found
    {
        let _ = stream.shutdown(Shutdown::Both);
        return Err(WireError::WrongServer { found });
    }
    let (s, r) = split(stream, hs.into_stateless_transport_mode()?)?;
    Ok((s, r, found))
}

impl Sender {
    /// Sends one frame.
    pub fn send(&mut self, payload: &[u8]) -> Result<(), WireError> {
        self.send_start(payload.len(), payload)
    }

    /// Sends the length of a frame of `len` bytes and then only `start` of it (`start` shorter
    /// than `len`): for testing how the other side treats a sender that stalls.
    #[doc(hidden)]
    pub fn send_start(&mut self, len: usize, start: &[u8]) -> Result<(), WireError> {
        let len = u32::try_from(len).map_err(|_| WireError::TooLarge(len, u32::MAX as usize))?;
        let mut plain = Vec::with_capacity(start.len() + 4);
        plain.extend_from_slice(&len.to_be_bytes());
        plain.extend_from_slice(start);
        let mut cipher = vec![0u8; MAX_NOISE];
        for chunk in plain.chunks(MAX_CHUNK) {
            let n = self.state.write_message(self.nonce, chunk, &mut cipher)?;
            self.nonce = self.nonce.checked_add(1).ok_or(WireError::Damaged)?;
            write_noise(&mut self.stream, cipher.get(..n).ok_or(WireError::Damaged)?)?;
        }
        Ok(())
    }

    /// Closes the connection both ways (the receiving half's read fails at once).
    pub fn shutdown(&self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }

    pub fn set_write_timeout(&self, t: Option<std::time::Duration>) -> std::io::Result<()> {
        self.stream.set_write_timeout(t)
    }
}

impl Receiver {
    /// Reads one frame of at most `max` bytes.
    pub fn recv(&mut self, max: usize) -> Result<Vec<u8>, WireError> {
        self.recv_with(max, &|_| true)
    }

    /// [`Receiver::recv`], asking `reserve` for room for the frame once its length is known: a
    /// frame refused room is [`WireError::NoRoom`] (the connection is no use after it). A frame
    /// given room must then arrive within [`frame_deadline`], or it is [`WireError::TooSlow`]:
    /// someone sending a large frame a byte at a time cannot hold its room for good.
    pub fn recv_with(&mut self, max: usize, reserve: &dyn Fn(usize) -> bool) -> Result<Vec<u8>, WireError> {
        self.recv_within(max, reserve, frame_deadline)
    }

    fn recv_within(
        &mut self,
        max: usize,
        reserve: &dyn Fn(usize) -> bool,
        time: fn(usize) -> std::time::Duration,
    ) -> Result<Vec<u8>, WireError> {
        let idle = self.stream.read_timeout().ok().flatten();
        let read = self.recv_until(max, reserve, time);
        let _ = self.stream.set_read_timeout(idle);
        read
    }

    fn recv_until(
        &mut self,
        max: usize,
        reserve: &dyn Fn(usize) -> bool,
        time: fn(usize) -> std::time::Duration,
    ) -> Result<Vec<u8>, WireError> {
        let mut plain: Vec<u8> = Vec::new();
        let mut want: Option<usize> = None;
        let mut deadline: Option<std::time::Instant> = None;
        let mut buf = vec![0u8; MAX_NOISE];
        loop {
            if let (Some(end), Some(len)) = (deadline, want) {
                let left = end.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    return Err(WireError::TooSlow(len));
                }
                self.stream.set_read_timeout(Some(left))?;
            }
            let msg = match read_noise(&mut self.stream) {
                Err(WireError::Io(e))
                    if deadline.is_some() && matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
                {
                    return Err(WireError::TooSlow(want.unwrap_or(0)));
                }
                other => other?,
            };
            if msg.len() < TAG {
                return Err(WireError::Damaged);
            }
            let n = self.state.read_message(self.nonce, &msg, &mut buf).map_err(|_| WireError::Damaged)?;
            self.nonce = self.nonce.checked_add(1).ok_or(WireError::Damaged)?;
            plain.extend_from_slice(buf.get(..n).ok_or(WireError::Damaged)?);
            if want.is_none() && plain.len() >= 4 {
                let head: [u8; 4] = plain.get(..4).and_then(|h| h.try_into().ok()).ok_or(WireError::Damaged)?;
                let len = usize::try_from(u32::from_be_bytes(head)).map_err(|_| WireError::Damaged)?;
                if len > max {
                    return Err(WireError::TooLarge(len, max));
                }
                if !reserve(len) {
                    return Err(WireError::NoRoom(len));
                }
                want = Some(len);
                deadline = std::time::Instant::now().checked_add(time(len));
            }
            if let Some(len) = want {
                let total = len.saturating_add(4);
                if plain.len() > total {
                    return Err(WireError::Damaged);
                }
                if plain.len() == total {
                    plain.drain(..4);
                    return Ok(plain);
                }
            }
        }
    }

    pub fn set_read_timeout(&self, t: Option<std::time::Duration>) -> std::io::Result<()> {
        self.stream.set_read_timeout(t)
    }

    pub fn peer(&self) -> Option<std::net::SocketAddr> {
        self.stream.peer_addr().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    type Server = Result<(Sender, Receiver), WireError>;
    type Client = Result<(Sender, Receiver, Fingerprint), WireError>;

    fn pair(trust: Trust) -> (Server, Client, Keys) {
        let keys = Keys::generate().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let k = keys.clone();
        let server = std::thread::spawn(move || accept(listener.accept().unwrap().0, &k));
        let client = connect(TcpStream::connect(addr).unwrap(), &trust);
        (server.join().unwrap(), client, keys)
    }

    #[test]
    fn frames_of_every_size_arrive_whole() {
        let (server, client, keys) = pair(Trust::FirstUse);
        let (mut ss, mut sr) = server.unwrap();
        let (mut cs, mut cr, fp) = client.unwrap();
        assert_eq!(fp, keys.fingerprint());
        for size in [0, 1, 3, 4, 5, MAX_CHUNK - 5, MAX_CHUNK - 4, MAX_CHUNK - 3, MAX_CHUNK, MAX_CHUNK * 3 + 7, 1_000_000] {
            let payload: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
            // Send on another thread: a frame larger than the socket buffers (about 64 KiB on
            // Windows) only finishes sending while the other end reads it.
            // If receiving fails, the socket is shut so the sender stops too (the test fails
            // instead of hanging).
            std::thread::scope(|t| {
                let sending = t.spawn(|| cs.send(&payload));
                let got = sr.recv(2_000_000);
                if got.as_ref().ok() != Some(&payload) {
                    let _ = sr.stream.shutdown(std::net::Shutdown::Both);
                }
                let sent = sending.join().unwrap();
                assert_eq!(got.unwrap(), payload, "{size} bytes to the server");
                sent.unwrap();
            });
            std::thread::scope(|t| {
                let sending = t.spawn(|| ss.send(&payload));
                let got = cr.recv(2_000_000);
                if got.as_ref().ok() != Some(&payload) {
                    let _ = cr.stream.shutdown(std::net::Shutdown::Both);
                }
                let sent = sending.join().unwrap();
                assert_eq!(got.unwrap(), payload, "{size} bytes to the client");
                sent.unwrap();
            });
        }
    }

    /// Regression: a frame given room was read for as long as its sender kept sending a byte a
    /// minute, holding its room in the server's budget for good.
    #[test]
    fn a_frame_that_stalls_is_given_up() {
        let (server, client, _) = pair(Trust::FirstUse);
        let (_, mut rx) = server.unwrap();
        let (mut tx, _, _) = client.unwrap();
        let idle = Some(std::time::Duration::from_secs(30));
        rx.set_read_timeout(idle).unwrap();
        tx.send_start(2 * 1024 * 1024, b"{\"type\":").unwrap();
        let started = std::time::Instant::now();
        let read = rx.recv_within(4 * 1024 * 1024, &|_| true, |_| std::time::Duration::from_millis(300));
        assert!(matches!(read, Err(WireError::TooSlow(_))), "{read:?}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(rx.stream.read_timeout().unwrap(), idle, "the idle timeout is put back");
        assert_eq!(frame_deadline(10), std::time::Duration::from_secs(60));
        assert_eq!(frame_deadline(64 * 1024 * 1024), std::time::Duration::from_secs(256));
    }

    #[test]
    fn a_frame_over_the_limit_is_refused_before_it_is_read() {
        let (server, client, _) = pair(Trust::FirstUse);
        let (_, mut sr) = server.unwrap();
        let (mut cs, _, _) = client.unwrap();
        // The receiver stops after the length, so the rest may never fit the socket buffers
        // (about 64 KiB on Windows): send on another thread, which ends when the socket closes.
        let sending = std::thread::spawn(move || cs.send(&vec![7u8; 70_000]));
        assert!(matches!(sr.recv(65_536), Err(WireError::TooLarge(70_000, 65_536))));
        drop(sr);
        let _ = sending.join().unwrap();
    }

    #[test]
    fn a_pinned_client_refuses_another_server() {
        let other = Keys::generate().unwrap().fingerprint();
        let (_, client, keys) = pair(Trust::Pinned(other));
        match client {
            Err(WireError::WrongServer { found }) => assert_eq!(found, keys.fingerprint()),
            other => panic!("expected WrongServer, got {:?}", other.map(|c| c.2)),
        }
        let (_, client, keys) = pair(Trust::Pinned(Fingerprint("placeholder".into())));
        assert!(client.is_err());
        assert!(keys.fingerprint().matches(&keys.fingerprint().0.to_uppercase().replace(' ', "")));
    }

    #[test]
    fn tampered_bytes_are_refused() {
        let keys = Keys::generate().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let k = keys.clone();
        let server = std::thread::spawn(move || {
            let (_, mut r) = accept(listener.accept().unwrap().0, &k).unwrap();
            r.recv(1000)
        });
        // A client that completes the handshake, then sends a message with a flipped bit.
        let mut stream = TcpStream::connect(addr).unwrap();
        let mut hs = Builder::new(params().unwrap()).prologue(PROLOGUE).unwrap().build_initiator().unwrap();
        let mut buf = vec![0u8; MAX_NOISE];
        let n = hs.write_message(&[], &mut buf).unwrap();
        write_noise(&mut stream, &buf[..n]).unwrap();
        let reply = read_noise(&mut stream).unwrap();
        hs.read_message(&reply, &mut buf).unwrap();
        let state = hs.into_stateless_transport_mode().unwrap();
        let n = state.write_message(0, b"\x00\x00\x00\x02hi", &mut buf).unwrap();
        buf[5] ^= 1;
        write_noise(&mut stream, &buf[..n]).unwrap();
        assert!(matches!(server.join().unwrap(), Err(WireError::Damaged)));
    }

    #[test]
    fn keys_are_saved_privately_and_load_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server").join("server-key.json");
        let a = Keys::load_or_create(&path).unwrap();
        let b = Keys::load_or_create(&path).unwrap();
        assert_eq!(a.fingerprint(), b.fingerprint());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::write(&path, "{\"private\":\"zz\",\"public\":\"00\"}").unwrap();
        assert!(Keys::load_or_create(&path).is_err());
        let other = Keys::generate().unwrap();
        let mixed = format!("{{\"private\":\"{}\",\"public\":\"{}\"}}", to_hex(&a.private), to_hex(&other.public));
        std::fs::write(&path, mixed).unwrap();
        assert!(Keys::load_or_create(&path).err().unwrap().contains("belong"));
    }

    #[test]
    fn hex_round_trips() {
        assert_eq!(from_hex(&to_hex(&[0, 1, 254, 255])), Some(vec![0, 1, 254, 255]));
        assert_eq!(from_hex("abc"), None);
        assert_eq!(from_hex("éé"), None);
    }
}
