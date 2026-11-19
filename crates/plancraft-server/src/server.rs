//! The server: accepts connections, signs people in and hands their requests to the shared plans.
//!
//! Plain threads, like the MCP server: one reader and one writer per connection, and one for
//! upkeep (saving plans and keeping their lock files fresh). A connection serves one plan once it
//! has opened it; its requests are handled in the order they came, one at a time. Nothing that
//! holds a plan's lock ever waits on a connection (see [`Outbox`]).

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, sync_channel};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use plancraft_core::guard;
use plancraft_core::io::DiskStamp;

use crate::Log;
use crate::plan::{Allowances, Broken, Budget, ConnId, Joined, Out, Outbox, SharedPlan, Subscriber};
use crate::protocol::{ClientMsg, MAX_CLIENT_FRAME, MAX_LOGIN_FRAME, PROTOCOL, PlanInfo, Role, ServerMsg};
use crate::users::{Gate, Throttle, User, Users};
use crate::wire::{self, Fingerprint, Keys};

/// Most connections at once. The target is 5–10 people, each with a few plans open; each
/// connection takes three file handles, and macOS gives a program 256 by default.
pub const MAX_CONNECTIONS: usize = 64;
/// Most connections from one address at once.
pub const MAX_PER_ADDRESS: usize = 16;
/// Time allowed from connecting to being signed in, in all (the default of
/// [`Options::login_timeout`]).
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(30);
/// Connections not signed in yet, at most: the other half of [`MAX_CONNECTIONS`] stays free
/// for people who have signed in.
pub const MAX_NOT_SIGNED_IN: usize = MAX_CONNECTIONS / 2;
/// Connections not signed in yet from one address, at most: holding every such slot takes
/// eight computers, not two.
pub const MAX_NOT_SIGNED_IN_PER_ADDRESS: usize = 4;
/// Requests allowed before signing in (hello, a few tries, pings).
const MAX_LOGIN_FRAMES: u32 = 10;
/// A signed-in connection that sends nothing for this long is closed (clients ping every 30 s).
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// A write that cannot go out for this long closes the connection.
const WRITE_TIMEOUT: Duration = Duration::from_secs(60);
/// Messages waiting for one connection, at most (bytes are limited too: see `plan::Outbox`).
const QUEUE: usize = 4096;
/// Requests that are not understood before the connection is closed.
const MAX_BAD_FRAMES: u32 = 20;
/// Most plans listed.
const MAX_LISTED: usize = 10_000;
/// Longest plan file name accepted.
const MAX_PLAN_NAME_BYTES: usize = 255;
/// Password checks at once.
const HASHING_AT_ONCE: usize = 2;
/// How long stopping waits for connections to end.
const STOP_WAIT: Duration = Duration::from_secs(5);

/// How to run a server.
#[derive(Clone)]
pub struct Options {
    /// The folder whose plans are shared.
    pub folder: PathBuf,
    /// The server's private folder (its key and users file).
    pub config: PathBuf,
    pub bind: SocketAddr,
    pub log: Log,
    /// Time allowed from connecting to being signed in.
    pub login_timeout: Duration,
    /// Connections not signed in yet from one address, at most.
    pub not_signed_in_per_address: usize,
    /// Bytes of large frames read at once from all connections ([`MAX_INBOUND_BYTES`]).
    pub max_inbound_bytes: usize,
    /// Announce the server on the office network (mDNS), so *Open from Server* lists it. Off by
    /// default (tests); the command line turns it on unless `--no-announce`.
    pub announce: bool,
}

impl Options {
    pub fn new(folder: PathBuf, config: PathBuf, bind: SocketAddr, log: Log) -> Self {
        Options {
            folder,
            config,
            bind,
            log,
            login_timeout: LOGIN_TIMEOUT,
            not_signed_in_per_address: MAX_NOT_SIGNED_IN_PER_ADDRESS,
            max_inbound_bytes: MAX_INBOUND_BYTES,
            announce: false,
        }
    }
}

/// The server's key file in its private folder.
pub fn key_path(config: &Path) -> PathBuf {
    config.join("server-key.json")
}

/// The users file in the server's private folder.
pub fn users_path(config: &Path) -> PathBuf {
    config.join("users.json")
}

/// The default private folder: `server` in PlanCraft's per-user configuration folder.
pub fn default_config() -> Option<PathBuf> {
    plancraft_core::paths::config_dir().map(|d| d.join("server"))
}

enum Slot {
    /// Being opened by the first person to ask.
    Empty,
    Open(Box<SharedPlan>),
    /// Closed (the last person left, or it broke): ask the map again.
    Closed,
}

/// One connection's own things, for handling its requests.
struct Ctx<'a> {
    shared: &'a Arc<Shared>,
    conn: ConnId,
    ip: Option<IpAddr>,
    out: &'a Outbox,
    receiver: &'a wire::Receiver,
}

/// The stopping of a server, for [`Running::wait`] to wait on.
#[derive(Default)]
struct Stopped {
    done: Mutex<bool>,
    signal: Condvar,
}

struct Shared {
    folder: PathBuf,
    users: PathBuf,
    /// The users file as last read, and its stamp: read again when it changes, so a removed
    /// person or a changed role takes effect on open connections too.
    users_cache: Mutex<Option<(Option<DiskStamp>, Users)>>,
    keys: Keys,
    name: String,
    plans: Mutex<HashMap<String, Arc<Mutex<Slot>>>>,
    throttle: Mutex<Throttle>,
    gate: Gate,
    next_conn: AtomicU64,
    connections: AtomicUsize,
    per_address: Mutex<HashMap<IpAddr, usize>>,
    closers: Mutex<HashMap<ConnId, Arc<TcpStream>>>,
    /// Connections not signed in yet, and since when (closed by upkeep when too slow).
    not_signed_in: Mutex<HashMap<ConnId, (Instant, IpAddr)>>,
    login_timeout: Duration,
    not_signed_in_per_address: usize,
    budget: Arc<Budget>,
    /// Bytes of large frames being read from all connections together (see [`MAX_INBOUND_BYTES`]).
    inbound: AtomicUsize,
    max_inbound: usize,
    /// Accounts with a large frame being read now: one each, so one account (on many
    /// connections) cannot take the whole budget.
    large_senders: Mutex<HashSet<String>>,
    allowances: Allowances,
    /// Counts up whenever the users file was read again.
    users_generation: AtomicU64,
    /// The server's announcement on the network, withdrawn first thing when stopping.
    announcer: Mutex<Option<crate::discovery::Announcer>>,
    stop: AtomicBool,
    stopped: Stopped,
    log: Log,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    /// The users file now (read again when it changed on disk).
    fn users(&self) -> Result<Users, String> {
        let stamp = plancraft_core::io::disk_stamp(&self.users);
        let mut cache = lock(&self.users_cache);
        if let Some((s, users)) = &*cache
            && *s == stamp
        {
            return Ok(users.clone());
        }
        let users = Users::load(&self.users)?;
        *cache = Some((stamp, users.clone()));
        self.users_generation.fetch_add(1, Ordering::SeqCst);
        Ok(users)
    }

    /// `user` as the users file has them now (`None`: removed).
    fn fresh(&self, user: &User) -> Option<User> {
        self.users().ok()?.get(&user.name).cloned()
    }
}

/// A running server.
pub struct Running {
    addr: SocketAddr,
    fingerprint: Fingerprint,
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

/// Starts a server: binds the address and serves until [`Running::stop`].
pub fn start(options: Options) -> Result<Running, String> {
    let folder = std::fs::canonicalize(&options.folder).map_err(|e| format!("cannot open the folder {}: {e}", options.folder.display()))?;
    if !folder.is_dir() {
        return Err(format!("{} is not a folder", folder.display()));
    }
    plancraft_core::io::create_private_dir(&options.config).map_err(|e| format!("cannot create {}: {e}", options.config.display()))?;
    let keys = Keys::load_or_create(&key_path(&options.config))?;
    let users = users_path(&options.config);
    if Users::load(&users)?.is_empty() {
        return Err(format!(
            "nobody can sign in yet: add someone first with `plancraft-server user add NAME --role admin` (users are kept in {})",
            users.display()
        ));
    }
    let listener = TcpListener::bind(options.bind).map_err(|e| format!("cannot listen on {}: {e}", options.bind))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let shared = Arc::new(Shared {
        folder,
        users,
        users_cache: Mutex::new(None),
        name: host_name(),
        keys,
        plans: Mutex::new(HashMap::new()),
        throttle: Mutex::new(Throttle::with_trusted_file(crate::users::trusted_path(&options.config))),
        gate: Gate::new(HASHING_AT_ONCE),
        next_conn: AtomicU64::new(1),
        connections: AtomicUsize::new(0),
        per_address: Mutex::new(HashMap::new()),
        closers: Mutex::new(HashMap::new()),
        not_signed_in: Mutex::new(HashMap::new()),
        login_timeout: options.login_timeout,
        not_signed_in_per_address: options.not_signed_in_per_address,
        budget: Budget::standard(),
        inbound: AtomicUsize::new(0),
        large_senders: Mutex::new(HashSet::new()),
        max_inbound: options.max_inbound_bytes,
        allowances: Allowances::default(),
        users_generation: AtomicU64::new(0),
        announcer: Mutex::new(None),
        stop: AtomicBool::new(false),
        stopped: Stopped::default(),
        log: options.log,
    });
    let fingerprint = shared.keys.fingerprint();
    if options.announce {
        let folder_name = shared.folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let name = crate::discovery::instance_name(&folder_name, &shared.name);
        match crate::discovery::Announcer::start(&name, &shared.name, addr) {
            Ok(a) => {
                (shared.log)(&format!("Announced on the network as \"{name}\""));
                *lock(&shared.announcer) = Some(a);
            }
            // Not fatal: people can still type the address.
            Err(e) => (shared.log)(&e),
        }
    }
    let mut threads = Vec::new();
    let s = Arc::clone(&shared);
    threads.push(spawn("plancraft-server-accept", move || accept_loop(&s, &listener)).map_err(|e| e.to_string())?);
    let s = Arc::clone(&shared);
    threads.push(spawn("plancraft-server-upkeep", move || upkeep_loop(&s)).map_err(|e| e.to_string())?);
    Ok(Running { addr, fingerprint, shared, threads })
}

/// This computer's name, as the server gives it (no control characters, at most 80 characters).
pub fn host_name() -> String {
    let name: String = ::gethostname::gethostname().to_string_lossy().chars().filter(|c| !c.is_control()).take(80).collect();
    if name.trim().is_empty() { "PlanCraft Server".into() } else { name }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new().name(name.into()).spawn(f)
}

/// A handle that stops a running server from another thread (a Ctrl+C handler).
#[derive(Clone)]
pub struct Stopper {
    shared: Arc<Shared>,
    addr: SocketAddr,
}

impl Stopper {
    /// Stops the server: no new connections or plans, every plan closed (saved, lock given back,
    /// everyone told), then every connection closed.
    pub fn stop(&self) {
        if self.shared.stop.swap(true, Ordering::SeqCst) {
            return;
        }
        // Withdrawn first, so nobody picks the server from a list while it stops.
        drop(lock(&self.shared.announcer).take());
        // Wake the accept loop.
        let wake = match self.addr.ip() {
            IpAddr::V4(ip) if ip.is_unspecified() => SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), self.addr.port()),
            IpAddr::V6(ip) if ip.is_unspecified() => SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), self.addr.port()),
            _ => self.addr,
        };
        let _ = TcpStream::connect_timeout(&wake, Duration::from_secs(2));
        // Joins check the stop flag under the map's lock, so none starts after this.
        let slots: Vec<(String, Arc<Mutex<Slot>>)> = lock(&self.shared.plans).drain().collect();
        for (_, slot) in slots {
            let mut guard = lock(&slot);
            if let Slot::Open(plan) = std::mem::replace(&mut *guard, Slot::Closed) {
                plan.close("The server is shutting down.");
            }
        }
        // Let the closing messages go out, then end every connection.
        let deadline = Instant::now() + STOP_WAIT;
        let mut first = true;
        while self.shared.connections.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
            if !first {
                for stream in lock(&self.shared.closers).values() {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
            }
            first = false;
            std::thread::sleep(Duration::from_millis(50));
        }
        *lock(&self.shared.stopped.done) = true;
        self.shared.stopped.signal.notify_all();
    }
}

impl Running {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn fingerprint(&self) -> &Fingerprint {
        &self.fingerprint
    }

    pub fn stopper(&self) -> Stopper {
        Stopper { shared: Arc::clone(&self.shared), addr: self.addr }
    }

    /// Stops the server (see [`Stopper::stop`]) and waits for its threads.
    pub fn stop(mut self) {
        self.stopper().stop();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }

    /// Waits until the server is stopped by a [`Stopper`], and the stopping is done (every plan
    /// saved and closed).
    pub fn wait(mut self) {
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        let mut done = lock(&self.shared.stopped.done);
        while !*done {
            done = self.shared.stopped.signal.wait(done).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

fn accept_loop(shared: &Arc<Shared>, listener: &TcpListener) {
    for stream in listener.incoming() {
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = stream else {
            // Out of file handles, for instance: wait a moment rather than spin.
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        let Ok(peer) = stream.peer_addr().map(|a| a.ip()) else { continue };
        // Every connection must be closable at once (stopping, a slow reader, a lost access).
        let Ok(socket) = stream.try_clone().map(Arc::new) else { continue };
        let conn = shared.next_conn.fetch_add(1, Ordering::SeqCst);
        {
            // Counted before the connection's thread starts, so a burst cannot get past the caps.
            let mut waiting = lock(&shared.not_signed_in);
            let from_here = waiting.values().filter(|(_, ip)| *ip == peer).count();
            if shared.connections.load(Ordering::SeqCst) >= MAX_CONNECTIONS
                || waiting.len() >= MAX_NOT_SIGNED_IN
                || from_here >= shared.not_signed_in_per_address
            {
                drop(waiting);
                (shared.log)(&format!("refused a connection from {peer}: too many at once"));
                continue;
            }
            let mut per = lock(&shared.per_address);
            let n = per.entry(peer).or_insert(0);
            if *n >= MAX_PER_ADDRESS {
                drop(per);
                drop(waiting);
                (shared.log)(&format!("refused a connection from {peer}: too many from there at once"));
                continue;
            }
            *n += 1;
            waiting.insert(conn, (Instant::now(), peer));
            shared.connections.fetch_add(1, Ordering::SeqCst);
        }
        lock(&shared.closers).insert(conn, Arc::clone(&socket));
        let done = move |s: &Shared| {
            lock(&s.not_signed_in).remove(&conn);
            lock(&s.closers).remove(&conn);
            s.connections.fetch_sub(1, Ordering::SeqCst);
            let mut per = lock(&s.per_address);
            if let Some(n) = per.get_mut(&peer) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    per.remove(&peer);
                }
            }
        };
        let s = Arc::clone(shared);
        let started = spawn("plancraft-server-conn", move || {
            let _ = guard::catch("a connection", || serve(&s, conn, stream, Some(socket)));
            done(&s);
        });
        if started.is_err() {
            done(shared);
        }
    }
}

/// Frames up to this size are read without asking for room.
const SMALL_FRAME: usize = 1024 * 1024;
/// Bytes of large frames read at once from all connections together, at most: a frame that does
/// not fit closes its connection (the client sends it again after reconnecting).
pub const MAX_INBOUND_BYTES: usize = 256 * 1024 * 1024;

/// Room taken in [`Shared::inbound`] (and the account's place in [`Shared::large_senders`]),
/// given back when dropped.
struct Room<'a> {
    shared: &'a Shared,
    bytes: usize,
    user: Option<String>,
}

impl Drop for Room<'_> {
    fn drop(&mut self) {
        if self.bytes > 0 {
            self.shared.inbound.fetch_sub(self.bytes, Ordering::SeqCst);
        }
        if let Some(user) = &self.user {
            lock(&self.shared.large_senders).remove(user);
        }
    }
}

/// Writes the trusted sign-ins list when it changed, outside the throttle's lock.
fn save_trusted(shared: &Shared) {
    let save = lock(&shared.throttle).take_trusted_save();
    if let Some((path, bytes)) = save
        && let Err(e) = plancraft_core::io::write_atomic(&path, &bytes, plancraft_core::io::Privacy::OwnerOnly)
    {
        (shared.log)(&format!("could not save the trusted sign-ins list: {e}"));
    }
}

fn upkeep_loop(shared: &Arc<Shared>) {
    let mut users_seen = shared.users_generation.load(Ordering::SeqCst);
    while !shared.stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(200));
        save_trusted(shared);
        // Connections that take too long to sign in (however slowly they send) are closed.
        let late: Vec<ConnId> = {
            let now = Instant::now();
            lock(&shared.not_signed_in)
                .iter()
                .filter(|(_, (t, _))| now.saturating_duration_since(*t) > shared.login_timeout)
                .map(|(c, _)| *c)
                .collect()
        };
        for conn in late {
            if let Some(sock) = lock(&shared.closers).get(&conn) {
                let _ = sock.shutdown(std::net::Shutdown::Both);
            }
        }
        // The users file changed: people removed or no longer allowed leave their plans now,
        // not at their next request (they might only be listening).
        let _ = shared.users();
        let generation = shared.users_generation.load(Ordering::SeqCst);
        if generation != users_seen {
            users_seen = generation;
            if let Ok(users) = shared.users() {
                let slots: Vec<Arc<Mutex<Slot>>> = lock(&shared.plans).values().cloned().collect();
                for slot in slots {
                    if let Slot::Open(plan) = &mut *lock(&slot) {
                        let name = plan.name().to_string();
                        plan.revoke(|user| users.get(user).map_or(Role::None, |u| u.role_for(&name)));
                    }
                }
            }
        }
        let slots: Vec<(String, Arc<Mutex<Slot>>)> = lock(&shared.plans).iter().map(|(k, v)| (k.clone(), Arc::clone(v))).collect();
        let now = Instant::now();
        for (name, slot) in slots {
            let outcome = guard::catch("plan upkeep", || match &mut *lock(&slot) {
                Slot::Open(plan) => match plan.maintain(now) {
                    Ok(()) if plan.is_empty() => Some(None),
                    Ok(()) => None,
                    Err(broken) => Some(Some(broken)),
                },
                _ => None,
            });
            match outcome {
                // A plan nobody has open (its last connection ended without leaving): close it.
                Ok(Some(None)) => close_if_empty(shared, &name, &slot),
                Ok(Some(Some(broken))) => break_plan(shared, &name, &slot, &broken),
                _ => {}
            }
        }
    }
    // A sign-in in the last moments is still remembered.
    save_trusted(shared);
}

/// Where a connection is.
enum Stage {
    Hello,
    SignIn,
    SignedIn(User),
    Open { user: User, name: String, slot: Arc<Mutex<Slot>>, role: Role },
}

fn writer(mut sender: wire::Sender, rx: &Receiver<Out>) {
    let _ = sender.set_write_timeout(Some(WRITE_TIMEOUT));
    for out in rx.iter() {
        match out {
            // A frame counts its bytes back when it is dropped, here or with the queue.
            Out::Frame(frame) => {
                if sender.send(frame.bytes()).is_err() {
                    break;
                }
            }
            Out::Close => break,
        }
    }
    sender.shutdown();
}

fn serve(shared: &Arc<Shared>, conn: ConnId, stream: TcpStream, socket: Option<Arc<TcpStream>>) {
    let started = Instant::now();
    let _ = stream.set_read_timeout(Some(shared.login_timeout));
    let _ = stream.set_write_timeout(Some(shared.login_timeout));
    let ip = stream.peer_addr().ok().map(|a| a.ip());
    let Ok((sender, mut receiver)) = wire::accept(stream, &shared.keys) else { return };
    let (tx, rx) = sync_channel::<Out>(QUEUE);
    let out = Outbox::new(conn, tx, Arc::clone(&shared.budget), socket);
    let Ok(writer_thread) = spawn("plancraft-server-send", move || writer(sender, &rx)) else { return };
    let mut stage = Stage::Hello;
    let mut bad = 0u32;
    let mut before_sign_in = 0u32;
    loop {
        // Small frames only, except from an editor with a plan open (a change can be large).
        let max = match &stage {
            Stage::Open { role, .. } if role.can_edit() => MAX_CLIENT_FRAME,
            _ => MAX_LOGIN_FRAME,
        };
        // A large frame (a big change) needs room in the budget for all connections: one editor
        // account cannot make the server hold many 65 MB frames (and what they parse into) at once.
        // A frame given room must arrive within `wire::frame_deadline`, so a slow sender cannot
        // keep it.
        let reserved = std::cell::Cell::new(0usize);
        let sender_name = match &stage {
            Stage::Open { user, .. } => Some(user.name.to_lowercase()),
            _ => None,
        };
        let holds_place = std::cell::Cell::new(false);
        let reserve = |len: usize| {
            if len <= SMALL_FRAME {
                return true;
            }
            if let Some(name) = &sender_name {
                if !lock(&shared.large_senders).insert(name.clone()) {
                    return false;
                }
                holds_place.set(true);
            }
            let before = shared.inbound.fetch_add(len, Ordering::SeqCst);
            if before.saturating_add(len) > shared.max_inbound {
                shared.inbound.fetch_sub(len, Ordering::SeqCst);
                return false;
            }
            reserved.set(len);
            true
        };
        let read = receiver.recv_with(max, &reserve);
        // Given back once the request was handled (the end of this turn of the loop).
        let _room = Room { shared, bytes: reserved.get(), user: sender_name.filter(|_| holds_place.get()) };
        let bytes = match read {
            Ok(bytes) => bytes,
            Err(wire::WireError::NoRoom(_)) => {
                out.push(&ServerMsg::Error {
                    message: "the server is busy reading other large changes; your change will be sent again shortly".into(),
                    for_good: false,
                });
                break;
            }
            Err(_) => break,
        };
        if matches!(stage, Stage::Hello | Stage::SignIn) {
            before_sign_in += 1;
            if before_sign_in > MAX_LOGIN_FRAMES || started.elapsed() > shared.login_timeout {
                out.push(&ServerMsg::Error { message: "signing in took too long".into(), for_good: false });
                break;
            }
        }
        let msg = match serde_json::from_slice::<ClientMsg>(&bytes) {
            Ok(m) => m,
            Err(e) => {
                bad += 1;
                out.push(&ServerMsg::Error { message: format!("the request was not understood: {e}"), for_good: false });
                if bad >= MAX_BAD_FRAMES {
                    break;
                }
                continue;
            }
        };
        let ctx = Ctx { shared, conn, ip, out: &out, receiver: &receiver };
        let handled = guard::catch("a request", || handle(&ctx, &mut stage, msg));
        match handled {
            Ok(true) => {}
            Ok(false) => break,
            Err(_) => {
                out.push(&ServerMsg::Error { message: "the server ran into a problem with that request".into(), for_good: false });
                break;
            }
        }
    }
    if let Stage::Open { name, slot, .. } = &stage {
        leave(shared, name, slot, conn);
    }
    out.finish();
    drop(out);
    let _ = writer_thread.join();
}

/// Handles one request. Returns whether the connection stays open.
fn handle(ctx: &Ctx, stage: &mut Stage, msg: ClientMsg) -> bool {
    let Ctx { shared, conn, ip, out, receiver } = *ctx;
    let error = |message: &str| {
        out.push(&ServerMsg::Error { message: message.into(), for_good: false });
    };
    // A refusal trying again will not change.
    let refuse = |message: &str| {
        out.push(&ServerMsg::Error { message: message.into(), for_good: true });
    };
    match (&*stage, msg) {
        (Stage::SignedIn(user), ClientMsg::Ping) => {
            // Someone removed from the users file stops here too, even if they only ping.
            if shared.fresh(user).is_none() {
                out.push(&ServerMsg::Closing { message: "you can no longer sign in to this server".into(), for_good: true });
                return false;
            }
            out.push(&ServerMsg::Pong);
        }
        (Stage::Open { user, name, .. }, ClientMsg::Ping) => {
            if shared.fresh(user).is_none_or(|u| u.role_for(name) == Role::None) {
                out.push(&ServerMsg::Closing { message: "you may no longer open this plan".into(), for_good: true });
                return false;
            }
            out.push(&ServerMsg::Pong);
        }
        (_, ClientMsg::Ping) => {
            out.push(&ServerMsg::Pong);
        }
        (Stage::Hello, ClientMsg::Hello { protocol, .. }) => {
            if protocol != PROTOCOL {
                refuse(&format!(
                    "this server speaks protocol {PROTOCOL} and this PlanCraft speaks {protocol}: use the same PlanCraft version on both computers"
                ));
                return false;
            }
            out.push(&ServerMsg::Welcome { protocol: PROTOCOL, server: shared.name.clone() });
            *stage = Stage::SignIn;
        }
        (Stage::Hello, _) => {
            error("say hello first");
            return false;
        }
        (Stage::SignIn, ClientMsg::Login { user, password }) => {
            if let Some(u) = sign_in(shared, ip, out, &user, &password.0) {
                let _ = receiver.set_read_timeout(Some(IDLE_TIMEOUT));
                (shared.log)(&format!("{} signed in from {}", u.name, ip.map(|i| i.to_string()).unwrap_or_default()));
                out.push(&ServerMsg::LoggedIn { user: u.name.clone() });
                lock(&shared.not_signed_in).remove(&conn);
                *stage = Stage::SignedIn(u);
            }
        }
        (Stage::SignIn, _) => error("sign in first"),
        (Stage::SignedIn(user), msg @ (ClientMsg::List | ClientMsg::Open { .. })) => {
            let Some(user) = shared.fresh(user) else {
                out.push(&ServerMsg::Closing { message: "you can no longer sign in to this server".into(), for_good: true });
                return false;
            };
            match msg {
                ClientMsg::List => {
                    out.push(&ServerMsg::Plans { plans: list(shared, &user) });
                }
                ClientMsg::Open { plan, client, known, in_flight } => {
                    let role = user.role_for(&plan);
                    if !valid_plan_name(&plan) || role == Role::None || !plan_exists(&shared.folder, &plan) {
                        refuse(&format!("there is no plan '{}' that you may open", printable(&plan)));
                        return true;
                    }
                    let sub = Subscriber { user: user.name.clone(), client, role, out: out.clone(), task: None };
                    match join(shared, &plan, conn, sub, known, in_flight) {
                        Ok(Some(slot)) => *stage = Stage::Open { user, name: plan, slot, role },
                        Ok(None) => {}
                        Err(e) => error(&e),
                    }
                }
                _ => {}
            }
        }
        (Stage::SignedIn(_), _) => error("open a plan first"),
        (Stage::Open { .. }, ClientMsg::Hello { .. } | ClientMsg::Login { .. } | ClientMsg::List | ClientMsg::Open { .. }) => {
            error("this connection already has a plan open");
        }
        (Stage::Open { user, name, slot, .. }, msg) => {
            // The users file as it is now: someone removed, or no longer allowed, stops here.
            let role = shared.fresh(user).map_or(Role::None, |u| u.role_for(name));
            if role == Role::None {
                out.push(&ServerMsg::Closing { message: "you may no longer open this plan".into(), for_good: true });
                return false;
            }
            let mut guard = lock(slot);
            let Slot::Open(plan) = &mut *guard else {
                out.push(&ServerMsg::Closing { message: "the plan was closed".into(), for_good: false });
                return false;
            };
            let result = match msg {
                ClientMsg::Change { change } if role.can_edit() => plan.submit(conn, &change),
                ClientMsg::Lease if role.can_edit() => plan.lease_for(conn),
                ClientMsg::Change { change } => {
                    out.push(&ServerMsg::Rejected {
                        change: change.id,
                        conflicts: Vec::new(),
                        reason: Some("you may only view this plan".into()),
                        retry: false,
                        role: Some(role),
                    });
                    Ok(())
                }
                ClientMsg::Lease => {
                    error("you may only view this plan");
                    Ok(())
                }
                ClientMsg::Resync => plan.resync(conn, false),
                ClientMsg::Presence { task } => {
                    plan.presence(conn, task);
                    Ok(())
                }
                _ => Ok(()),
            };
            drop(guard);
            if let Err(broken) = result {
                break_plan(shared, name, slot, &broken);
                return false;
            }
            // An editor turned viewer gets small frames only from now on.
            if let Stage::Open { role: r, .. } = stage {
                *r = role;
            }
        }
    }
    true
}

fn sign_in(shared: &Arc<Shared>, ip: Option<IpAddr>, out: &Outbox, user: &str, password: &str) -> Option<User> {
    // The try counts as failed from now on, so tries sent at once cannot all get in before the
    // first one fails.
    if let Err(wait) = lock(&shared.throttle).begin(user, ip) {
        let secs = wait.as_secs().max(1);
        out.push(&ServerMsg::LoginFailed {
            message: format!("too many failed sign-ins; try again in {secs} seconds"),
            retry_after_secs: secs,
            credentials: false,
        });
        return None;
    }
    let users = match shared.users() {
        Ok(u) => u,
        Err(e) => {
            (shared.log)(&format!("cannot read the users file: {e}"));
            out.push(&ServerMsg::LoginFailed {
                message: "the server cannot read its users file".into(),
                retry_after_secs: 0,
                credentials: false,
            });
            return None;
        }
    };
    let found = shared.gate.run(|| users.verify(user, password).cloned());
    let mut throttle = lock(&shared.throttle);
    match found {
        Some(u) => {
            throttle.succeeded(user, ip);
            Some(u)
        }
        None => {
            let retry = throttle.wait(user, ip).as_secs();
            drop(throttle);
            (shared.log)(&format!("failed sign-in as '{}' from {}", printable(user), ip.map(|i| i.to_string()).unwrap_or_default()));
            out.push(&ServerMsg::LoginFailed { message: "wrong user name or password".into(), retry_after_secs: retry, credentials: true });
            None
        }
    }
}

/// A name from the network, safe to show in a log or message.
fn printable(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(80).collect()
}

/// A plan file directly in the shared folder: no folders, no hidden files, no way out.
pub fn valid_plan_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_PLAN_NAME_BYTES
        && !name.starts_with('.')
        && !name.contains(['/', '\\', ':'])
        && !name.chars().any(char::is_control)
        && name.to_ascii_lowercase().ends_with(".plancraft")
        && Path::new(name).file_name().is_some_and(|f| f == name)
}

/// Whether `name` is a plan file in `folder`, spelled exactly as the folder spells it: on a
/// file system that ignores case, "plan.plancraft" and "Plan.plancraft" are one file, and must
/// never be shared as two.
fn plan_exists(folder: &Path, name: &str) -> bool {
    let Ok(dir) = std::fs::read_dir(folder) else { return false };
    dir.filter_map(Result::ok).any(|e| e.file_name().to_str() == Some(name) && e.file_type().is_ok_and(|t| t.is_file()))
}

fn list(shared: &Shared, user: &User) -> Vec<PlanInfo> {
    let open: HashMap<String, usize> = {
        let slots: Vec<(String, Arc<Mutex<Slot>>)> = lock(&shared.plans).iter().map(|(k, v)| (k.clone(), Arc::clone(v))).collect();
        slots
            .into_iter()
            .filter_map(|(k, slot)| match &*lock(&slot) {
                Slot::Open(p) => Some((k, p.people_count())),
                _ => None,
            })
            .collect()
    };
    let Ok(dir) = std::fs::read_dir(&shared.folder) else { return Vec::new() };
    let mut plans: Vec<PlanInfo> = dir
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| valid_plan_name(n))
        .take(MAX_LISTED)
        .filter_map(|name| {
            let role = user.role_for(&name);
            (role > Role::None).then(|| PlanInfo { people: open.get(&name).copied().unwrap_or(0), name, role })
        })
        .collect();
    plans.sort_by_key(|p| p.name.to_lowercase());
    plans
}

/// Opens the plan if nobody has it open yet, and lets the connection in. `Ok(None)`: refused,
/// with the reason sent.
fn join(
    shared: &Arc<Shared>,
    name: &str,
    conn: ConnId,
    sub: Subscriber,
    known: Option<crate::protocol::Known>,
    in_flight: Option<plancraft_core::sync::ChangeId>,
) -> Result<Option<Arc<Mutex<Slot>>>, String> {
    for _ in 0..5 {
        let slot = {
            let mut plans = lock(&shared.plans);
            // Checked under the map's lock: once stopping has emptied the map, nothing reopens.
            if shared.stop.load(Ordering::SeqCst) {
                return Err("the server is shutting down".into());
            }
            Arc::clone(plans.entry(name.to_string()).or_insert_with(|| Arc::new(Mutex::new(Slot::Empty))))
        };
        let mut guard = lock(&slot);
        if matches!(*guard, Slot::Empty) {
            match SharedPlan::open_with(&shared.folder.join(name), name, Arc::clone(&shared.log), Arc::clone(&shared.allowances)) {
                Ok(p) => *guard = Slot::Open(Box::new(p)),
                Err(e) => {
                    *guard = Slot::Closed;
                    drop(guard);
                    forget(shared, name, &slot);
                    (shared.log)(&format!("cannot share {name}: {e}"));
                    return Err(e);
                }
            }
        }
        let Slot::Open(plan) = &mut *guard else {
            // Closed while we waited: take it out of the map (if still there) and open afresh.
            drop(guard);
            forget(shared, name, &slot);
            continue;
        };
        match plan.join(conn, sub.clone(), known, in_flight) {
            Ok(Joined::Yes) => {
                drop(guard);
                return Ok(Some(slot));
            }
            Ok(Joined::No) => {
                let empty = plan.is_empty();
                drop(guard);
                if empty {
                    close_if_empty(shared, name, &slot);
                }
                return Ok(None);
            }
            Err(broken) => {
                drop(guard);
                break_plan(shared, name, &slot, &broken);
                return Err(broken.0);
            }
        }
    }
    Err(format!("{name} could not be opened; try again"))
}

/// Takes a closed plan out of the map (unless it was replaced meanwhile).
fn forget(shared: &Shared, name: &str, slot: &Arc<Mutex<Slot>>) {
    let mut plans = lock(&shared.plans);
    if plans.get(name).is_some_and(|s| Arc::ptr_eq(s, slot)) {
        plans.remove(name);
    }
}

fn leave(shared: &Arc<Shared>, name: &str, slot: &Arc<Mutex<Slot>>, conn: ConnId) {
    {
        let mut guard = lock(slot);
        let Slot::Open(plan) = &mut *guard else { return };
        plan.leave(conn);
    }
    close_if_empty(shared, name, slot);
}

/// Closes the plan (saving it and giving back its lock) when nobody has it open. The save runs
/// under the plan's own lock only, so other plans open and list meanwhile; someone opening this
/// plan waits for the save, finds it closed, and opens it afresh.
fn close_if_empty(shared: &Arc<Shared>, name: &str, slot: &Arc<Mutex<Slot>>) {
    {
        let mut guard = lock(slot);
        let Slot::Open(plan) = &mut *guard else { return };
        if !plan.is_empty() {
            return;
        }
        if let Slot::Open(plan) = std::mem::replace(&mut *guard, Slot::Closed) {
            plan.close("Closed");
        }
    }
    forget(shared, name, slot);
}

fn break_plan(shared: &Arc<Shared>, name: &str, slot: &Arc<Mutex<Slot>>, why: &Broken) {
    {
        let mut guard = lock(slot);
        if let Slot::Open(plan) = std::mem::replace(&mut *guard, Slot::Closed) {
            plan.abandon(why);
        }
    }
    forget(shared, name, slot);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_names_stay_inside_the_folder() {
        for good in ["Plan.plancraft", "Office move.plancraft", "été.PLANCRAFT"] {
            assert!(valid_plan_name(good), "{good}");
        }
        for bad in [
            "",
            ".hidden.plancraft",
            "../up.plancraft",
            "sub/plan.plancraft",
            "sub\\plan.plancraft",
            "C:plan.plancraft",
            "plan.txt",
            "..",
            "a\nb.plancraft",
            &"x".repeat(300),
        ] {
            assert!(!valid_plan_name(bad), "{bad:?}");
        }
    }
}
