//! `plancraft-server` (and `plancraft serve`): runs a server, or manages its users.

use std::io::BufRead;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use crate::Log;
use crate::protocol::{DEFAULT_PORT, Role};
use crate::server::{self, Options};
use crate::users::{NewPassword, Users};
use crate::wire::Keys;

const HELP: &str = "\
PlanCraft Server: shares a folder of plans so several people can edit them at once.
(`plancraft serve ...` runs the same server from the PlanCraft program.)

Usage:
  plancraft-server FOLDER [--port N] [--bind ADDRESS] [--config DIR]
      Shares the .plancraft files in FOLDER until stopped (Ctrl+C).
      --port N         the port to listen on (default 47340)
      --bind ADDRESS   the address to listen on (default 0.0.0.0, every network)
      --config DIR     the server's private folder: its key and users
                       (default: PlanCraft's settings folder, under 'server')
      --no-announce    don't announce the server on the network (people then
                       type its address in Open from Server)
      --watch-stdin    stop (saving every plan) when standard input closes, and
                       say on standard output when listening; PlanCraft's
                       File > Share Folder runs the server this way
      Put -- before FOLDER when its name starts with '-' or is a command word.

  plancraft-server user add NAME [--role viewer|editor|admin] [--password-stdin]
  plancraft-server user passwd NAME [--password-stdin]
  plancraft-server user role NAME ROLE [--plan FILE]
  plancraft-server user remove NAME
  plancraft-server user list
      Manages who may sign in (each command also takes --config DIR).
      Roles: viewer (opens plans), editor (also changes them), admin.
      With --plan, the role applies to that plan only ('none' hides it).

  plancraft-server fingerprint [--config DIR]
      Shows the server's key fingerprint, which people confirm on their first visit.

Nothing listens on the network unless a server is started like this.";

struct Args {
    rest: Vec<String>,
    /// `rest` as given (a folder whose name is not valid UTF-8 is still that folder).
    rest_os: Vec<std::ffi::OsString>,
    /// The first name came after `--`: it is the folder to share, even if it reads like a
    /// command (`help`, `user`, `--help`).
    folder_only: bool,
    config: Option<PathBuf>,
    port: Option<String>,
    bind: Option<String>,
    role: Option<String>,
    plan: Option<String>,
    password_stdin: bool,
    no_announce: bool,
    watch_stdin: bool,
}

fn parse(args: Vec<std::ffi::OsString>) -> Result<Args, String> {
    let mut out = Args {
        rest: Vec::new(),
        rest_os: Vec::new(),
        folder_only: false,
        config: None,
        port: None,
        bind: None,
        role: None,
        plan: None,
        password_stdin: false,
        no_announce: false,
        watch_stdin: false,
    };
    let mut it = args.into_iter();
    let mut options_end = false;
    while let Some(os) = it.next() {
        let a = os.to_string_lossy().into_owned();
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        let mut text = |name: &str| value(name).map(|v| v.to_string_lossy().into_owned());
        match a.as_str() {
            _ if options_end => {}
            "--" => {
                // Everything after it is a name, even one starting with "-".
                options_end = true;
                continue;
            }
            "--config" => {
                out.config = Some(PathBuf::from(value("--config")?));
                continue;
            }
            "--port" => {
                out.port = Some(text("--port")?);
                continue;
            }
            "--bind" => {
                out.bind = Some(text("--bind")?);
                continue;
            }
            "--role" => {
                out.role = Some(text("--role")?);
                continue;
            }
            "--plan" => {
                out.plan = Some(text("--plan")?);
                continue;
            }
            "--password-stdin" => {
                out.password_stdin = true;
                continue;
            }
            "--no-announce" => {
                out.no_announce = true;
                continue;
            }
            "--watch-stdin" => {
                out.watch_stdin = true;
                continue;
            }
            "--help" | "--version" => {}
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            _ => {}
        }
        if options_end && out.rest.is_empty() {
            out.folder_only = true;
        }
        out.rest.push(a);
        out.rest_os.push(os);
    }
    Ok(out)
}

fn config(args: &Args) -> Result<PathBuf, String> {
    args.config
        .clone()
        .or_else(server::default_config)
        .ok_or_else(|| "cannot tell where to keep the server's files: pass --config DIR".into())
}

fn stderr_log() -> Log {
    Arc::new(|line: &str| {
        let when = plancraft_core::dates::format("%Y-%m-%d %H:%M:%S", plancraft_core::dates::now());
        // Never `eprintln!`: it panics when standard error is gone (the PlanCraft that started
        // the server quit), and the server must still save its plans then.
        use std::io::Write;
        let _ = writeln!(std::io::stderr().lock(), "{when}  {line}");
    })
}

/// Prints a line of output. Never `println!`: it panics when the output is closed (`… | head`),
/// after the change was made.
fn say(text: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stdout().lock(), "{text}");
}

fn read_password(args: &Args, prompt: &str) -> Result<String, String> {
    if args.password_stdin {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).map_err(|e| e.to_string())?;
        return Ok(line.trim_end_matches(['\r', '\n']).to_string());
    }
    let first = rpassword::prompt_password(prompt).map_err(|e| format!("cannot read the password: {e}"))?;
    let again = rpassword::prompt_password("Type it again: ").map_err(|e| format!("cannot read the password: {e}"))?;
    if first != again {
        return Err("the two passwords differ".into());
    }
    Ok(first)
}

/// Runs the command line; returns the exit code.
pub fn run(args: Vec<std::ffi::OsString>) -> i32 {
    match run_inner(args) {
        Ok(()) => 0,
        Err(e) => {
            use std::io::Write;
            // One line (a folder name may hold a line break): PlanCraft's Share Folder reads the
            // last line as why the server stopped.
            let e: String = e.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
            let _ = writeln!(std::io::stderr().lock(), "plancraft-server: {e}");
            1
        }
    }
}

fn run_inner(args: Vec<std::ffi::OsString>) -> Result<(), String> {
    let args = parse(args)?;
    if args.folder_only {
        return serve(&args);
    }
    match args.rest.first().map(String::as_str) {
        None | Some("help" | "-h" | "--help") => {
            say(HELP);
            Ok(())
        }
        Some("-V" | "--version") => {
            say(&format!("plancraft-server {}", env!("CARGO_PKG_VERSION")));
            Ok(())
        }
        Some("user") => user(&args),
        Some("fingerprint") => {
            let keys = Keys::load_or_create(&server::key_path(&config(&args)?))?;
            say(&keys.fingerprint().to_string());
            Ok(())
        }
        Some(_) => serve(&args),
    }
}

fn user(args: &Args) -> Result<(), String> {
    let path = server::users_path(&config(args)?);
    let arg = |i: usize, what: &str| args.rest.get(i).cloned().ok_or_else(|| format!("{what} is missing (see plancraft-server --help)"));
    let role = |s: &str| Role::parse(s).ok_or_else(|| format!("'{s}' is not a role: use viewer, editor, admin or none"));
    match args.rest.get(1).map(String::as_str) {
        Some("add") => {
            let name = arg(2, "the user name")?;
            let r = role(args.role.as_deref().unwrap_or("editor"))?;
            let password = NewPassword::new(&read_password(args, &format!("Password for {name}: "))?)?;
            Users::change(&path, |users| users.add_with(&name, &password, r))?;
            say(&format!("Added {name} ({}).", r.name()));
        }
        Some("passwd") => {
            let name = arg(2, "the user name")?;
            let password = NewPassword::new(&read_password(args, &format!("New password for {name}: "))?)?;
            Users::change(&path, |users| users.set_password_with(&name, &password))?;
            say(&format!("Changed the password of {name}."));
        }
        Some("role") => {
            let name = arg(2, "the user name")?;
            let r = role(&arg(3, "the role")?)?;
            if let Some(p) = &args.plan
                && !server::valid_plan_name(p)
            {
                return Err(format!(
                    "'{p}' is not a plan's file name: give it as the shared folder shows it, e.g. 'Office move.plancraft'"
                ));
            }
            Users::change(&path, |users| users.set_role(&name, r, args.plan.as_deref()))?;
            match &args.plan {
                Some(p) => say(&format!("{name} is now {} for {p}.", r.name())),
                None => say(&format!("{name} is now {}.", r.name())),
            }
        }
        Some("remove") => {
            let name = arg(2, "the user name")?;
            Users::change(&path, |users| users.remove(&name))?;
            say(&format!("Removed {name}."));
        }
        Some("list") => {
            let users = Users::load(&path)?;
            if users.is_empty() {
                say("No users yet.");
            }
            for u in users.list() {
                let extra: Vec<String> = u.plans.iter().map(|(p, r)| format!("{p}: {}", r.name())).collect();
                if extra.is_empty() {
                    say(&format!("{}  {}", u.name, u.role.name()));
                } else {
                    say(&format!("{}  {}  ({})", u.name, u.role.name(), extra.join(", ")));
                }
            }
        }
        _ => return Err("use: user add|passwd|role|remove|list (see plancraft-server --help)".into()),
    }
    Ok(())
}

/// What a server run with `--watch-stdin` says on standard output once it listens: one line of
/// JSON (`{"ready":{"addr":"0.0.0.0:47340","fingerprint":"…"}}`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Ready {
    pub addr: SocketAddr,
    pub fingerprint: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ReadyLine {
    ready: Ready,
}

impl Ready {
    /// The line printed.
    pub fn line(&self) -> String {
        serde_json::to_string(&ReadyLine { ready: self.clone() }).unwrap_or_default()
    }

    /// Reads a line printed by [`Ready::line`] (`None` for any other line).
    pub fn parse(line: &str) -> Option<Ready> {
        serde_json::from_str::<ReadyLine>(line.trim()).ok().map(|l| l.ready)
    }
}

/// `--watch-stdin`: says it is ready on standard output, and stops the server once standard
/// input closes (the program that started it quit, or crashed).
fn watch_stdin(running: &server::Running, log: &Log) {
    use std::io::{Read, Write};
    let ready = Ready { addr: running.addr(), fingerprint: running.fingerprint().0.clone() };
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", ready.line()).and_then(|()| out.flush());
    let stopper = running.stopper();
    let on_close = log.clone();
    let watcher = std::thread::Builder::new().name("plancraft-server-stdin".into()).spawn(move || {
        let mut sink = [0u8; 256];
        let mut stdin = std::io::stdin().lock();
        loop {
            match stdin.read(&mut sink) {
                Ok(0) => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
                Ok(_) => {}
            }
        }
        on_close("Stopping: PlanCraft closed the server's input; saving the shared plans");
        stopper.stop();
    });
    if let Err(e) = watcher {
        // Without the watcher nothing would ever stop the server: stop now rather than run on
        // with no one able to stop it.
        log(&format!("cannot watch standard input ({e}); stopping"));
        running.stopper().stop();
    }
}

fn serve(args: &Args) -> Result<(), String> {
    let folder = PathBuf::from(args.rest_os.first().ok_or("which folder?")?);
    if args.rest.len() > 1 {
        return Err("give one folder to share".into());
    }
    let port = match &args.port {
        Some(p) => p.parse::<u16>().map_err(|_| format!("'{p}' is not a port number"))?,
        None => DEFAULT_PORT,
    };
    let ip = match &args.bind {
        Some(b) => b.parse::<IpAddr>().map_err(|_| format!("'{b}' is not an address"))?,
        None => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
    };
    plancraft_core::lock::set_app_name(format!("PlanCraft Server {}", env!("CARGO_PKG_VERSION")));
    let log = stderr_log();
    let mut options = Options::new(folder.clone(), config(args)?, SocketAddr::new(ip, port), log.clone());
    options.announce = !args.no_announce;
    let running = server::start(options)?;
    log(&format!("PlanCraft Server {} is sharing {}", env!("CARGO_PKG_VERSION"), folder.display()));
    log(&format!("Listening on {}", running.addr()));
    log(&format!("Key fingerprint: {}  (people check this when they first connect)", running.fingerprint()));
    if args.watch_stdin {
        watch_stdin(&running, &log);
    }
    // Ctrl+C, or a service manager stopping the server: save every plan, tell everyone, give
    // back the lock files.
    let stopper = running.stopper();
    let on_signal = log.clone();
    if let Err(e) = ctrlc::set_handler(move || {
        on_signal("Stopping: saving the shared plans");
        stopper.stop();
    }) {
        log(&format!("cannot catch Ctrl+C ({e}); stopping this way may leave plans to be recovered from their journals"));
    }
    running.wait();
    log("Stopped");
    Ok(())
}
