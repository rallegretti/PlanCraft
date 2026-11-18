//! `plancraft-server` (and `plancraft serve`): runs a server, or manages its users.

use std::io::BufRead;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use crate::Log;
use crate::protocol::{DEFAULT_PORT, Role};
use crate::server::{self, Options};
use crate::users::Users;
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
    config: Option<PathBuf>,
    port: Option<String>,
    bind: Option<String>,
    role: Option<String>,
    plan: Option<String>,
    password_stdin: bool,
}

fn parse(args: Vec<String>) -> Result<Args, String> {
    let mut out = Args { rest: Vec::new(), config: None, port: None, bind: None, role: None, plan: None, password_stdin: false };
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "--config" => out.config = Some(PathBuf::from(value("--config")?)),
            "--port" => out.port = Some(value("--port")?),
            "--bind" => out.bind = Some(value("--bind")?),
            "--role" => out.role = Some(value("--role")?),
            "--plan" => out.plan = Some(value("--plan")?),
            "--password-stdin" => out.password_stdin = true,
            "--help" | "--version" => out.rest.push(a),
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            _ => out.rest.push(a),
        }
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
        eprintln!("{when}  {line}");
    })
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
pub fn run(args: Vec<String>) -> i32 {
    match run_inner(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("plancraft-server: {e}");
            1
        }
    }
}

fn run_inner(args: Vec<String>) -> Result<(), String> {
    let args = parse(args)?;
    match args.rest.first().map(String::as_str) {
        None | Some("help" | "-h" | "--help") => {
            println!("{HELP}");
            Ok(())
        }
        Some("-V" | "--version") => {
            println!("plancraft-server {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("user") => user(&args),
        Some("fingerprint") => {
            let keys = Keys::load_or_create(&server::key_path(&config(&args)?))?;
            println!("{}", keys.fingerprint());
            Ok(())
        }
        Some(_) => serve(&args),
    }
}

fn user(args: &Args) -> Result<(), String> {
    let path = server::users_path(&config(args)?);
    let mut users = Users::load(&path)?;
    let arg = |i: usize, what: &str| args.rest.get(i).cloned().ok_or_else(|| format!("{what} is missing (see plancraft-server --help)"));
    let role = |s: &str| Role::parse(s).ok_or_else(|| format!("'{s}' is not a role: use viewer, editor, admin or none"));
    match args.rest.get(1).map(String::as_str) {
        Some("add") => {
            let name = arg(2, "the user name")?;
            let r = role(args.role.as_deref().unwrap_or("editor"))?;
            let password = read_password(args, &format!("Password for {name}: "))?;
            users.add(&name, &password, r)?;
            users.save()?;
            println!("Added {name} ({}).", r.name());
        }
        Some("passwd") => {
            let name = arg(2, "the user name")?;
            let password = read_password(args, &format!("New password for {name}: "))?;
            users.set_password(&name, &password)?;
            users.save()?;
            println!("Changed the password of {name}.");
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
            users.set_role(&name, r, args.plan.as_deref())?;
            users.save()?;
            match &args.plan {
                Some(p) => println!("{name} is now {} for {p}.", r.name()),
                None => println!("{name} is now {}.", r.name()),
            }
        }
        Some("remove") => {
            let name = arg(2, "the user name")?;
            users.remove(&name)?;
            users.save()?;
            println!("Removed {name}.");
        }
        Some("list") => {
            if users.is_empty() {
                println!("No users yet.");
            }
            for u in users.list() {
                let extra: Vec<String> = u.plans.iter().map(|(p, r)| format!("{p}: {}", r.name())).collect();
                if extra.is_empty() {
                    println!("{}  {}", u.name, u.role.name());
                } else {
                    println!("{}  {}  ({})", u.name, u.role.name(), extra.join(", "));
                }
            }
        }
        _ => return Err("use: user add|passwd|role|remove|list (see plancraft-server --help)".into()),
    }
    Ok(())
}

fn serve(args: &Args) -> Result<(), String> {
    let folder = PathBuf::from(args.rest.first().ok_or("which folder?")?);
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
    let running = server::start(Options::new(folder.clone(), config(args)?, SocketAddr::new(ip, port), log.clone()))?;
    log(&format!("PlanCraft Server {} is sharing {}", env!("CARGO_PKG_VERSION"), folder.display()));
    log(&format!("Listening on {}", running.addr()));
    log(&format!("Key fingerprint: {}  (people check this when they first connect)", running.fingerprint()));
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
