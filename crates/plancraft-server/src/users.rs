//! Who may sign in, and what they may do: the server's users file.
//!
//! The file (`users.json` in the server's private folder) is readable by its owner only and holds
//! Argon2id password hashes, never passwords. Each person has a role for the whole folder and may
//! have other roles for single plans. The server reads the file again for every sign-in, so
//! `plancraft-server user …` takes effect without a restart.
//!
//! Failed sign-ins are slowed down per user name and per address ([`Throttle`]), and at most a
//! few passwords are checked at once, so guessing passwords or flooding the server with sign-ins
//! gets nowhere.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::{Argon2, PasswordHash};
use plancraft_core::io::{Privacy, write_atomic};
use serde::{Deserialize, Serialize};

use crate::protocol::Role;

/// Most bytes of a users file read.
const MAX_USERS_FILE: u64 = 4 * 1024 * 1024;
pub const MAX_USERS: usize = 1_000;
pub const MAX_USER_NAME_CHARS: usize = 64;
pub const MIN_PASSWORD_CHARS: usize = 8;
pub const MAX_PASSWORD_CHARS: usize = 1_024;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub name: String,
    /// The password's Argon2id hash in PHC form (`$argon2id$v=19$…`).
    pub hash: String,
    /// The role for every plan in the folder.
    pub role: Role,
    /// Other roles for single plans, by file name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plans: BTreeMap<String, Role>,
}

impl std::fmt::Debug for User {
    // Never the hash: it would let someone guess the password offline.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("User").field("name", &self.name).field("role", &self.role).field("plans", &self.plans).finish_non_exhaustive()
    }
}

impl User {
    /// The role for one plan. Plan names are compared ignoring case: a rule written as
    /// `secret.plancraft` must not miss the file `Secret.plancraft` where the file system
    /// ignores case (and two plans told apart only by case share their rules, never escape them).
    /// Should a hand-edited file hold two rules for one plan, the stricter one applies.
    pub fn role_for(&self, plan: &str) -> Role {
        let plan = plan_key(plan);
        self.plans.iter().filter(|(p, _)| plan_key(p) == plan).map(|(_, r)| *r).min().unwrap_or(self.role)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct UsersFile {
    #[serde(default)]
    users: Vec<User>,
}

/// The users file, loaded.
#[derive(Clone)]
pub struct Users {
    path: PathBuf,
    users: Vec<User>,
}

/// A plan name as rules compare it: one Unicode form (macOS file systems store accented names
/// decomposed, and people type them composed) and one case.
pub fn plan_key(name: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    name.nfc().collect::<String>().to_lowercase()
}

/// Makes a private file private again if someone loosened it (best effort, Unix).
pub fn keep_private(path: &Path, meta: &std::fs::Metadata) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
    }
    #[cfg(not(unix))]
    let _ = (path, meta);
}

/// A name as people type it: trimmed, with no control characters.
pub fn clean_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("a user name cannot be empty".into());
    }
    if name.chars().count() > MAX_USER_NAME_CHARS {
        return Err(format!("a user name can have at most {MAX_USER_NAME_CHARS} characters"));
    }
    if name.chars().any(char::is_control) {
        return Err("a user name cannot hold control characters".into());
    }
    Ok(name.to_string())
}

fn same_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

fn check_password(password: &str) -> Result<(), String> {
    let n = password.chars().count();
    if n < MIN_PASSWORD_CHARS {
        return Err(format!("a password needs at least {MIN_PASSWORD_CHARS} characters"));
    }
    if n > MAX_PASSWORD_CHARS {
        return Err(format!("a password can have at most {MAX_PASSWORD_CHARS} characters"));
    }
    Ok(())
}

fn hash_password(password: &str) -> Result<String, String> {
    Argon2::default().hash_password(password.as_bytes()).map(|h| h.to_string()).map_err(|e| format!("cannot hash the password: {e}"))
}

/// A hash of no one's password, checked against for unknown names so a sign-in takes as long
/// whether or not the name exists.
fn decoy_hash() -> &'static str {
    static DECOY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DECOY.get_or_init(|| hash_password("decoy password, never valid").unwrap_or_default())
}

fn verify_hash(hash: &str, password: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
}

impl Users {
    /// Loads the users file at `path` (no file: no users yet).
    pub fn load(path: &Path) -> Result<Users, String> {
        let users = match std::fs::symlink_metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
            Ok(m) if !m.is_file() => return Err(format!("{} is not a regular file", path.display())),
            Ok(m) => {
                keep_private(path, &m);
                let mut text = String::new();
                std::fs::File::open(path)
                    .and_then(|f| f.take(MAX_USERS_FILE + 1).read_to_string(&mut text))
                    .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
                if text.len() as u64 > MAX_USERS_FILE {
                    return Err(format!("{} is too large", path.display()));
                }
                let file: UsersFile = serde_json::from_str(&text).map_err(|e| format!("{} is damaged: {e}", path.display()))?;
                file.users.into_iter().take(MAX_USERS).collect()
            }
        };
        Ok(Users { path: path.to_path_buf(), users })
    }

    pub fn save(&self) -> Result<(), String> {
        let text = serde_json::to_string_pretty(&UsersFile { users: self.users.clone() }).map_err(|e| e.to_string())?;
        if let Some(dir) = self.path.parent() {
            plancraft_core::io::create_private_dir(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        write_atomic(&self.path, text.as_bytes(), Privacy::OwnerOnly).map_err(|e| format!("cannot write {}: {e}", self.path.display()))
    }

    pub fn list(&self) -> &[User] {
        &self.users
    }

    pub fn is_empty(&self) -> bool {
        self.users.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&User> {
        self.users.iter().find(|u| same_name(&u.name, name))
    }

    fn get_mut(&mut self, name: &str) -> Result<&mut User, String> {
        self.users.iter_mut().find(|u| same_name(&u.name, name)).ok_or_else(|| format!("there is no user '{}'", name.trim()))
    }

    pub fn add(&mut self, name: &str, password: &str, role: Role) -> Result<(), String> {
        let name = clean_name(name)?;
        if self.get(&name).is_some() {
            return Err(format!("there is already a user '{name}'"));
        }
        if self.users.len() >= MAX_USERS {
            return Err(format!("a server can have at most {MAX_USERS} users"));
        }
        check_password(password)?;
        let hash = hash_password(password)?;
        self.users.push(User { name, hash, role, plans: BTreeMap::new() });
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> Result<(), String> {
        let before = self.users.len();
        self.users.retain(|u| !same_name(&u.name, name));
        if self.users.len() == before { Err(format!("there is no user '{}'", name.trim())) } else { Ok(()) }
    }

    pub fn set_password(&mut self, name: &str, password: &str) -> Result<(), String> {
        check_password(password)?;
        let hash = hash_password(password)?;
        self.get_mut(name)?.hash = hash;
        Ok(())
    }

    /// Sets someone's role for the whole folder, or (with `plan`) for one plan.
    pub fn set_role(&mut self, name: &str, role: Role, plan: Option<&str>) -> Result<(), String> {
        let user = self.get_mut(name)?;
        match plan {
            Some(p) => {
                // One rule per plan, whatever the case it was written in (see `User::role_for`).
                let key = plan_key(p);
                user.plans.retain(|existing, _| plan_key(existing) != key);
                user.plans.insert(p.to_string(), role);
            }
            None => user.role = role,
        }
        Ok(())
    }

    /// Takes away someone's rule for one plan, so the role for the whole folder applies to it.
    pub fn clear_plan_role(&mut self, name: &str, plan: &str) -> Result<(), String> {
        let user = self.get_mut(name)?;
        let key = plan_key(plan);
        let before = user.plans.len();
        user.plans.retain(|existing, _| plan_key(existing) != key);
        if user.plans.len() == before { Err(format!("{} has no rule for {plan}", user.name)) } else { Ok(()) }
    }

    /// The user, if the password is theirs. Takes as long for an unknown name.
    pub fn verify(&self, name: &str, password: &str) -> Option<&User> {
        if password.chars().count() > MAX_PASSWORD_CHARS {
            return None;
        }
        match self.get(name) {
            Some(u) => verify_hash(&u.hash, password).then_some(u),
            None => {
                let _ = verify_hash(decoy_hash(), password);
                None
            }
        }
    }
}

/// Failed sign-ins allowed before waits begin.
const FREE_FAILURES: u32 = 5;
/// The longest wait.
const MAX_WAIT: Duration = Duration::from_secs(15 * 60);
/// Failures are forgotten after this long without another.
const FORGET_AFTER: Duration = Duration::from_secs(60 * 60);
/// Most names and addresses tracked (old ones are forgotten first).
const MAX_TRACKED: usize = 10_000;

#[derive(Clone, Copy, Debug)]
struct Failures {
    count: u32,
    last: Instant,
}

impl Failures {
    fn wait(&self, now: Instant) -> Duration {
        if self.count < FREE_FAILURES {
            return Duration::ZERO;
        }
        let exp = (self.count - FREE_FAILURES).min(20);
        let wait = Duration::from_secs(1u64 << exp).min(MAX_WAIT);
        wait.saturating_sub(now.saturating_duration_since(self.last))
    }
}

/// Slows down failed sign-ins: after five failures for one name from one address, or from one
/// address for any names, each try must wait twice as long as the one before (1 s, 2 s, 4 s … up
/// to 15 minutes).
///
/// A try counts as failed from the moment it starts ([`Throttle::begin`]) and is forgiven when the
/// password was right, so many tries sent at once cannot all slip through before the first fails.
/// Names are tracked per address, so a stranger cannot lock someone out from elsewhere.
///
/// One name guessed from many addresses waits too, except at addresses where that name signed in
/// recently: otherwise someone spending one failed try a minute could keep the real person out
/// for good.
#[derive(Debug, Default)]
pub struct Throttle {
    failures: HashMap<String, Failures>,
    /// When each name last signed in from each network (`user:name@net:…`: an IPv4 address, or an
    /// IPv6 /64, since a computer's temporary IPv6 addresses change within it).
    trusted: HashMap<String, SystemTime>,
    /// Where the trusted list is kept (owner-only), so a restart does not lose it.
    trusted_file: Option<PathBuf>,
    /// The trusted list changed since it was last handed out to be written.
    trusted_unsaved: bool,
}

/// The trusted list's file in the server's private folder.
pub fn trusted_path(config: &Path) -> PathBuf {
    config.join("trusted-sign-ins.json")
}

/// The largest trusted list file read.
const MAX_TRUSTED_BYTES: u64 = 4 * 1024 * 1024;

fn trust_key(user: &str, ip: Option<IpAddr>) -> String {
    let net = match ip {
        Some(IpAddr::V6(v6)) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let [a, b, c, d, ..] = v6.segments();
                format!("{a:x}:{b:x}:{c:x}:{d:x}::/64")
            }
        },
        Some(IpAddr::V4(v4)) => v4.to_string(),
        None => "unknown".into(),
    };
    format!("user:{}@net:{net}", name_key(user))
}

fn address_key(ip: Option<IpAddr>) -> String {
    // Exact addresses: on an office network every computer shares one IPv6 /64 (and fe80::/64),
    // so grouping by network would let one person's typos lock out everyone.
    match ip {
        Some(IpAddr::V6(v6)) => format!("ip:{}", v6.to_ipv4_mapped().map_or_else(|| v6.to_string(), |v4| v4.to_string())),
        Some(IpAddr::V4(v4)) => format!("ip:{v4}"),
        None => "ip:unknown".into(),
    }
}

fn keys(user: &str, ip: Option<IpAddr>) -> [String; 2] {
    let addr = address_key(ip);
    [format!("user:{}@{addr}", name_key(user)), addr]
}

fn name_key(user: &str) -> String {
    user.trim().to_lowercase().chars().take(MAX_USER_NAME_CHARS).collect()
}

/// Failed sign-ins for one name from all addresses together before that name waits too.
const NAME_FREE_FAILURES: u32 = 30;
/// The longest such wait: someone guessing from many addresses is slowed to a try a minute.
const NAME_MAX_WAIT: Duration = Duration::from_secs(60);
/// How long a sign-in keeps the name-wide wait away from its network (kept in
/// [`trusted_path`] across restarts).
const TRUSTED_FOR: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// Failed sign-ins for one name from inside a trusted network before that network loses its
/// trust for the name. A whole IPv6 /64 is trusted, and one computer can use any address in it,
/// so the per-address count alone would let it guess without the name-wide wait.
const TRUSTED_NET_FAILURES: u32 = 10;
/// The longest trusted-list entry kept (a 64-character name of 4-byte characters and a network).
const MAX_TRUST_KEY_BYTES: usize = 400;

impl Throttle {
    /// A throttle keeping its trusted list in `path` (read now, written on each sign-in). A
    /// missing, oversized or unreadable file is an empty list.
    pub fn with_trusted_file(path: PathBuf) -> Throttle {
        let now = SystemTime::now();
        let trusted = plancraft_core::io::open_regular(&path, false, plancraft_core::io::Links::Refuse)
            .ok()
            .and_then(|f| {
                let mut text = String::new();
                std::io::Read::take(f, MAX_TRUSTED_BYTES).read_to_string(&mut text).ok()?;
                serde_json::from_str::<HashMap<String, u64>>(&text).ok()
            })
            .unwrap_or_default()
            .into_iter()
            .filter(|(k, _)| k.len() <= MAX_TRUST_KEY_BYTES)
            .filter_map(|(k, secs)| Some((k, SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs))?)))
            .filter(|(_, t)| now.duration_since(*t).is_ok_and(|age| age < TRUSTED_FOR))
            .take(MAX_TRACKED)
            .collect();
        Throttle { failures: HashMap::new(), trusted, trusted_file: Some(path), trusted_unsaved: false }
    }

    /// The trusted list to write, when it changed since the last call: the file and its bytes.
    /// The caller writes it after letting go of the throttle, so sign-ins never wait on the disk.
    pub fn take_trusted_save(&mut self) -> Option<(PathBuf, Vec<u8>)> {
        if !std::mem::take(&mut self.trusted_unsaved) {
            return None;
        }
        let path = self.trusted_file.clone()?;
        let list: HashMap<&String, u64> =
            self.trusted.iter().filter_map(|(k, t)| Some((k, t.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs()))).collect();
        Some((path, serde_json::to_vec(&list).ok()?))
    }

    /// How long `user` from `ip` must wait before trying again (zero: may try now).
    pub fn wait(&self, user: &str, ip: Option<IpAddr>) -> Duration {
        let now = Instant::now();
        let local = keys(user, ip).iter().filter_map(|k| self.failures.get(k)).map(|f| f.wait(now)).max().unwrap_or(Duration::ZERO);
        if self.is_trusted(user, ip) {
            return local;
        }
        let everywhere = self.failures.get(&format!("name:{}", name_key(user))).map_or(Duration::ZERO, |f| {
            if f.count < NAME_FREE_FAILURES {
                return Duration::ZERO;
            }
            NAME_MAX_WAIT.saturating_sub(now.saturating_duration_since(f.last))
        });
        local.max(everywhere)
    }

    /// Whether `user` signed in from `ip`'s network lately, and has not failed often from there
    /// since (never for an unknown address).
    fn is_trusted(&self, user: &str, ip: Option<IpAddr>) -> bool {
        let now = SystemTime::now();
        let key = trust_key(user, ip);
        let failing = self.failures.get(&format!("failed:{key}")).is_some_and(|f| f.count >= TRUSTED_NET_FAILURES);
        ip.is_some() && !failing && self.trusted.get(&key).is_some_and(|t| now.duration_since(*t).is_ok_and(|age| age < TRUSTED_FOR))
    }

    /// Starts a try: counts it as failed until [`Throttle::succeeded`]. `Err` with the wait when
    /// it must not be tried now.
    pub fn begin(&mut self, user: &str, ip: Option<IpAddr>) -> Result<(), Duration> {
        let wait = self.wait(user, ip);
        if !wait.is_zero() {
            return Err(wait);
        }
        self.failed(user, ip);
        Ok(())
    }

    pub fn failed(&mut self, user: &str, ip: Option<IpAddr>) {
        let now = Instant::now();
        self.failures.retain(|_, f| now.saturating_duration_since(f.last) < FORGET_AFTER);
        // Up to four entries are added below.
        while self.failures.len() + 4 > MAX_TRACKED {
            let Some(oldest) = self.failures.iter().min_by_key(|(_, f)| f.last).map(|(k, _)| k.clone()) else { break };
            self.failures.remove(&oldest);
        }
        let [a, b] = keys(user, ip);
        for k in [a, b, format!("name:{}", name_key(user)), format!("failed:{}", trust_key(user, ip))] {
            let f = self.failures.entry(k).or_insert(Failures { count: 0, last: now });
            f.count = f.count.saturating_add(1);
            f.last = now;
        }
    }

    /// The password was right: the try begun is forgiven, and the name's failures from there are
    /// cleared (not the address's other failures: one person may be guessing many names there).
    pub fn succeeded(&mut self, user: &str, ip: Option<IpAddr>) {
        let [name, addr] = keys(user, ip);
        self.failures.remove(&name);
        if ip.is_some() {
            let now = SystemTime::now();
            self.trusted.retain(|_, t| now.duration_since(*t).is_ok_and(|age| age < TRUSTED_FOR));
            while self.trusted.len() >= MAX_TRACKED {
                let Some(oldest) = self.trusted.iter().min_by_key(|(_, t)| **t).map(|(k, _)| k.clone()) else { break };
                self.trusted.remove(&oldest);
            }
            self.trusted.insert(trust_key(user, ip), now);
            self.trusted_unsaved = true;
        }
        if let Some(f) = self.failures.get_mut(&format!("failed:{}", trust_key(user, ip))) {
            f.count = f.count.saturating_sub(1);
        }
        if let Some(f) = self.failures.get_mut(&format!("name:{}", name_key(user))) {
            f.count = f.count.saturating_sub(1);
        }
        if let Some(f) = self.failures.get_mut(&addr) {
            f.count = f.count.saturating_sub(1);
            if f.count == 0 {
                self.failures.remove(&addr);
            }
        }
    }
}

/// Lets at most a few password checks run at once (each takes memory and a moment of CPU).
pub struct Gate {
    busy: Mutex<usize>,
    freed: Condvar,
    max: usize,
}

impl Gate {
    pub fn new(max: usize) -> Self {
        Gate { busy: Mutex::new(0), freed: Condvar::new(), max: max.max(1) }
    }

    pub fn run<T>(&self, f: impl FnOnce() -> T) -> T {
        {
            let mut busy = self.busy.lock().unwrap_or_else(PoisonError::into_inner);
            while *busy >= self.max {
                busy = self.freed.wait(busy).unwrap_or_else(PoisonError::into_inner);
            }
            *busy += 1;
        }
        let out = f();
        let mut busy = self.busy.lock().unwrap_or_else(PoisonError::into_inner);
        *busy = busy.saturating_sub(1);
        self.freed.notify_one();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_verify_change_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server/users.json");
        let mut users = Users::load(&path).unwrap();
        assert!(users.is_empty());
        users.add("Ana", "correct horse", Role::Editor).unwrap();
        assert!(users.add("ana ", "another one", Role::Viewer).is_err(), "names are compared ignoring case");
        assert!(users.add("Ben", "short", Role::Viewer).is_err(), "passwords need 8 characters");
        assert!(users.add("", "long enough", Role::Viewer).is_err());
        assert!(users.add("tab\tname", "long enough", Role::Viewer).is_err());
        users.set_role("ana", Role::Viewer, Some("Secret.plancraft")).unwrap();
        // A rule taken away (whatever case it is named in) lets the folder's role apply again.
        users.set_role("ana", Role::None, Some("Gone.plancraft")).unwrap();
        users.clear_plan_role("ana", "GONE.plancraft").unwrap();
        assert!(users.clear_plan_role("ana", "Gone.plancraft").is_err());
        assert!(users.clear_plan_role("nobody", "Secret.plancraft").is_err());
        assert_eq!(users.get("ana").unwrap().role_for("Gone.plancraft"), Role::Editor);
        users.save().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("correct horse"), "only the hash is saved");
        assert!(text.contains("$argon2id$"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }

        let mut users = Users::load(&path).unwrap();
        let ana = users.verify("ANA", "correct horse").unwrap();
        assert_eq!(ana.role_for("Other.plancraft"), Role::Editor);
        assert_eq!(ana.role_for("Secret.plancraft"), Role::Viewer);
        assert!(users.verify("Ana", "wrong horse").is_none());
        assert!(users.verify("Nobody", "correct horse").is_none());
        users.set_password("Ana", "battery staple").unwrap();
        assert!(users.verify("Ana", "correct horse").is_none());
        assert!(users.verify("Ana", "battery staple").is_some());
        users.remove("ana").unwrap();
        assert!(users.remove("ana").is_err());
        assert!(users.verify("Ana", "battery staple").is_none());
    }

    #[test]
    fn a_damaged_or_odd_users_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("users.json");
        std::fs::write(&path, "{not json").unwrap();
        assert!(Users::load(&path).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(Users::load(&path).is_err());
        let path = dir.path().join("hash.json");
        std::fs::write(&path, r#"{"users":[{"name":"x","hash":"garbage","role":"admin"}]}"#).unwrap();
        assert!(Users::load(&path).unwrap().verify("x", "anything at all").is_none(), "a damaged hash never matches");
    }

    #[test]
    fn failures_slow_down_by_name_and_by_address() {
        let mut t = Throttle::default();
        let ip: IpAddr = "10.0.0.7".parse().unwrap();
        for _ in 0..FREE_FAILURES {
            assert_eq!(t.wait("ana", Some(ip)), Duration::ZERO);
            t.failed("ana", Some(ip));
        }
        assert!(t.wait("ana", Some(ip)) > Duration::ZERO, "the name waits there");
        assert!(t.wait("ben", Some(ip)) > Duration::ZERO, "so does the address, for any name");
        let elsewhere: IpAddr = "10.0.0.8".parse().unwrap();
        assert_eq!(t.wait("ana", Some(elsewhere)), Duration::ZERO, "nobody is locked out from another computer");
        // A try that began (counted as failed) and turned out right.
        t.begin("ana", Some(elsewhere)).unwrap();
        t.failed("ana", Some(ip));
        t.succeeded("ANA", Some(ip));
        assert!(t.wait("ana", Some(ip)) > Duration::ZERO, "the address still waits for its other failures");
        for _ in 0..100 {
            t.failed("carl", None);
        }
        assert!(t.wait("carl", None) <= MAX_WAIT);
    }

    /// Regression: the trust that keeps a real person out of the name-wide wait was lost on a
    /// restart, and with each new temporary IPv6 address: someone failing once a minute for that
    /// name kept them waiting at every new address.
    #[test]
    fn trust_outlives_a_restart_and_a_new_temporary_address() {
        let dir = tempfile::tempdir().unwrap();
        let file = trusted_path(dir.path());
        let home: IpAddr = "2001:db8:1:2::10".parse().unwrap();
        let mut t = Throttle::with_trusted_file(file.clone());
        t.begin("ana", Some(home)).unwrap();
        t.succeeded("ana", Some(home));
        // Someone guesses "ana" from many other addresses.
        for i in 0..NAME_FREE_FAILURES * 2 {
            let ip: IpAddr = format!("10.9.{}.{}", i / 200, i % 200).parse().unwrap();
            t.failed("ana", Some(ip));
        }
        let renewed: IpAddr = "2001:db8:1:2:abcd::99".parse().unwrap();
        assert_eq!(t.wait("ana", Some(renewed)), Duration::ZERO, "a new temporary address in the same network is still trusted");
        assert!(t.wait("ana", Some("2001:db8:9:9::1".parse().unwrap())) > Duration::ZERO, "another network waits");
        // After a restart the trust is still there.
        let (path, bytes) = t.take_trusted_save().unwrap();
        assert!(t.take_trusted_save().is_none(), "handed out once per change");
        plancraft_core::io::write_atomic(&path, &bytes, plancraft_core::io::Privacy::OwnerOnly).unwrap();
        let mut again = Throttle::with_trusted_file(file.clone());
        for i in 0..NAME_FREE_FAILURES * 2 {
            let ip: IpAddr = format!("10.8.{}.{}", i / 200, i % 200).parse().unwrap();
            again.failed("ana", Some(ip));
        }
        assert_eq!(again.wait("ana", Some(home)), Duration::ZERO);
        // A damaged file is an empty list, never an error.
        std::fs::write(&file, "{not json").unwrap();
        assert!(Throttle::with_trusted_file(file).trusted.is_empty());
    }

    /// Regression: trusting a whole IPv6 /64 let a computer in it guess a trusted name from a new
    /// address each time, never meeting the name-wide wait.
    #[test]
    fn guessing_from_inside_a_trusted_network_loses_the_trust() {
        let mut t = Throttle::default();
        let home: IpAddr = "2001:db8:1:2::10".parse().unwrap();
        t.begin("ana", Some(home)).unwrap();
        t.succeeded("ana", Some(home));
        let refused = (0..200u32)
            .filter(|i| {
                let ip: IpAddr = format!("2001:db8:1:2:{:x}::1", i + 1).parse().unwrap();
                t.begin("ana", Some(ip)).is_err()
            })
            .count();
        assert!(refused > 150, "guesses rotating through the trusted /64 must meet the name-wide wait ({refused} refused)");
        // A few typos from the real person's network keep the trust.
        let mut t = Throttle::default();
        t.begin("ana", Some(home)).unwrap();
        t.succeeded("ana", Some(home));
        for i in 0..NAME_FREE_FAILURES * 2 {
            t.failed("ana", Some(format!("10.7.{}.{}", i / 200, i % 200).parse().unwrap()));
        }
        for _ in 0..TRUSTED_NET_FAILURES - 1 {
            t.failed("ana", Some(home));
        }
        assert!(t.is_trusted("ana", Some(home)));
    }

    #[test]
    fn tries_count_from_the_start_so_parallel_ones_cannot_slip_through() {
        let mut t = Throttle::default();
        let ip: IpAddr = "10.0.0.9".parse().unwrap();
        let started = (0..60).filter(|_| t.begin("ana", Some(ip)).is_ok()).count();
        assert_eq!(started, FREE_FAILURES as usize);
        // A right password forgives its own try.
        let mut t = Throttle::default();
        for _ in 0..10 {
            t.begin("ana", Some(ip)).unwrap();
            t.succeeded("ana", Some(ip));
        }
        assert_eq!(t.wait("ana", Some(ip)), Duration::ZERO);
    }

    #[test]
    fn neighbours_on_one_network_do_not_share_their_waits() {
        let mut t = Throttle::default();
        for _ in 0..FREE_FAILURES * 3 {
            t.failed("ana", Some("fe80::1".parse().unwrap()));
        }
        assert!(t.wait("ben", Some("fe80::1".parse().unwrap())) > Duration::ZERO);
        assert_eq!(t.wait("ben", Some("fe80::2".parse().unwrap())), Duration::ZERO, "the next computer on the network is not locked out");
        assert!(t.wait("x", Some("::ffff:10.0.0.1".parse().unwrap())) == t.wait("x", Some("10.0.0.1".parse().unwrap())));
        let long = "x".repeat(70_000);
        t.failed(&long, None);
        assert!(t.failures.keys().all(|k| k.len() < 200), "names are cut short");
    }

    #[test]
    fn guessing_one_name_from_many_addresses_is_slowed_too() {
        let mut t = Throttle::default();
        for i in 0..NAME_FREE_FAILURES {
            let ip: IpAddr = format!("fd00::{i:x}").parse().unwrap();
            t.failed("ana", Some(ip));
        }
        let fresh: IpAddr = "fd00::ffff".parse().unwrap();
        let wait = t.wait("ana", Some(fresh));
        assert!(wait > Duration::ZERO && wait <= NAME_MAX_WAIT, "{wait:?}");
        assert_eq!(t.wait("ben", Some(fresh)), Duration::ZERO);
    }

    #[test]
    fn guessing_from_many_addresses_never_locks_out_where_the_person_signs_in() {
        let mut t = Throttle::default();
        let home: IpAddr = "10.0.0.5".parse().unwrap();
        t.begin("ana", Some(home)).unwrap();
        t.succeeded("ana", Some(home));
        for i in 0..NAME_FREE_FAILURES * 4 {
            let ip: IpAddr = format!("fd00::{i:x}").parse().unwrap();
            t.failed("ana", Some(ip));
            assert_eq!(t.wait("ana", Some(home)), Duration::ZERO, "ana can still sign in where she always does");
        }
        assert!(t.begin("ana", Some(home)).is_ok());
        let new_place: IpAddr = "fd01::1".parse().unwrap();
        assert!(t.wait("ana", Some(new_place)) > Duration::ZERO, "a new address still waits");
        assert!(t.wait("ana", None) > Duration::ZERO, "so does an unknown one");
    }

    #[test]
    fn rules_match_names_in_either_unicode_form() {
        let dir = tempfile::tempdir().unwrap();
        let mut users = Users::load(&dir.path().join("u.json")).unwrap();
        users.add("eve", "correct horse", Role::Editor).unwrap();
        users.set_role("eve", Role::None, Some("Caf\u{e9}.plancraft")).unwrap();
        let eve = users.get("eve").unwrap();
        assert_eq!(eve.role_for("Cafe\u{301}.plancraft"), Role::None, "the decomposed name a Mac lists");
        users.set_role("eve", Role::Viewer, Some("CAFE\u{301}.PLANCRAFT")).unwrap();
        assert_eq!(users.get("eve").unwrap().plans.len(), 1);
    }

    #[test]
    fn a_rule_written_in_another_case_replaces_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut users = Users::load(&dir.path().join("u.json")).unwrap();
        users.add("eve", "correct horse", Role::Viewer).unwrap();
        users.set_role("eve", Role::Editor, Some("Secret.plancraft")).unwrap();
        users.set_role("eve", Role::None, Some("secret.plancraft")).unwrap();
        let eve = users.get("eve").unwrap();
        assert_eq!(eve.role_for("Secret.plancraft"), Role::None);
        assert_eq!(eve.plans.len(), 1);
    }

    #[test]
    fn two_rules_for_one_plan_in_a_hand_edited_file_give_the_stricter_role() {
        let eve = User {
            name: "eve".into(),
            hash: String::new(),
            role: Role::Editor,
            plans: [("Secret.plancraft".to_string(), Role::Editor), ("secret.plancraft".to_string(), Role::None)].into_iter().collect(),
        };
        assert_eq!(eve.role_for("SECRET.plancraft"), Role::None);
        assert_eq!(eve.role_for("Other.plancraft"), Role::Editor);
    }

    #[test]
    fn the_hash_never_shows_in_debug_output() {
        let dir = tempfile::tempdir().unwrap();
        let mut users = Users::load(&dir.path().join("u.json")).unwrap();
        users.add("ana", "correct horse", Role::Editor).unwrap();
        let shown = format!("{:?}", users.get("ana").unwrap());
        assert!(!shown.contains("argon2"), "{shown}");
    }

    #[test]
    fn the_throttle_forgets_the_oldest_when_full() {
        let mut t = Throttle::default();
        for i in 0..MAX_TRACKED + 10 {
            t.failed(&format!("user{i}"), None);
        }
        assert!(t.failures.len() <= MAX_TRACKED);
    }

    #[test]
    fn the_gate_lets_a_few_through_at_once() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let gate = Arc::new(Gate::new(2));
        let now = Arc::new(AtomicUsize::new(0));
        let most = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (gate, now, most) = (gate.clone(), now.clone(), most.clone());
                std::thread::spawn(move || {
                    gate.run(|| {
                        let n = now.fetch_add(1, Ordering::SeqCst) + 1;
                        most.fetch_max(n, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(20));
                        now.fetch_sub(1, Ordering::SeqCst);
                    })
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert!(most.load(Ordering::SeqCst) <= 2);
    }
}
