//! Command-line entry point shared by the `plancraft-mcp` binary and `plancraft mcp`.
//!
//! ```text
//! plancraft-mcp [FILE...] [--http ADDR] [--no-file-access]
//! ```

use std::process::ExitCode;
use std::sync::Arc;

use crate::{DEFAULT_HTTP_ADDR, McpServer, ServerOptions, run_stdio, serve_http, workspace_from_files};

const USAGE: &str = "\
plancraft-mcp — Model Context Protocol server for PlanCraft projects

USAGE:
    plancraft-mcp [FILE...] [--http [ADDR]] [--no-file-access] [--read-only]
    plancraft-mcp --attach

ARGS:
    FILE...             .plancraft documents to open (a missing file becomes a new,
                        unsaved project at that path). With no files the server starts
                        with no document; agents can call new_project / open_project.

OPTIONS:
    --http [ADDR]       Serve MCP Streamable HTTP on ADDR (default 127.0.0.1:7878)
                        at http://ADDR/mcp instead of stdio. A new bearer token is
                        written at startup to an owner-only file (its path is printed);
                        clients send 'Authorization: Bearer <token>'.
    --attach            Serve MCP on stdio by forwarding to the running PlanCraft
                        desktop app (Project > AI Agents > Start server).
    --no-file-access    Refuse tools that open, create or link arbitrary file paths.
    --read-only         Offer only the tools that read: agents can look at the plans
                        but not change, save or export them.
    -h, --help          Show this help.
    -V, --version       Show the version.

EXAMPLES:
    claude mcp add plancraft -- plancraft-mcp /path/to/project.plancraft
    claude mcp add plancraft -- plancraft-mcp --attach
    plancraft-mcp project.plancraft --http 127.0.0.1:7878
";

struct Cli {
    files: Vec<String>,
    http: Option<String>,
    file_access: bool,
    read_only: bool,
    attach: bool,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Option<Cli>, String> {
    let mut cli = Cli { files: Vec::new(), http: None, file_access: true, read_only: false, attach: false };
    let mut args = args.peekable();
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("plancraft-mcp {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--http" => {
                let explicit = args.next_if(|next| !next.starts_with('-') && !next.ends_with(".plancraft") && next.contains(':'));
                cli.http = Some(explicit.unwrap_or_else(|| DEFAULT_HTTP_ADDR.to_string()));
            }
            "--no-file-access" => cli.file_access = false,
            "--read-only" => cli.read_only = true,
            "--attach" => cli.attach = true,
            "--stdio" => cli.http = None,
            s if s.starts_with("--http=") => cli.http = s.strip_prefix("--http=").map(str::to_string),
            s if s.starts_with('-') && s.len() > 1 => {
                return Err(format!("unknown option '{s}'\n\n{USAGE}"));
            }
            _ => cli.files.push(a),
        }
    }
    Ok(Some(cli))
}

/// Runs the MCP server CLI with the given arguments (program name excluded).
pub fn run(args: Vec<String>) -> ExitCode {
    let cli = match parse_args(args.into_iter()) {
        Ok(Some(c)) => c,
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("plancraft-mcp: {e}");
            return ExitCode::from(2);
        }
    };
    if cli.attach {
        if cli.http.is_some() || !cli.files.is_empty() {
            eprintln!("plancraft-mcp: --attach works with the documents open in the app; it takes no files or --http");
            return ExitCode::from(2);
        }
        if cli.read_only {
            eprintln!("plancraft-mcp: --attach uses the app's server: tick Read only in Project > AI Agents instead of --read-only");
            return ExitCode::from(2);
        }
        return match crate::attach::run_attach() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("plancraft-mcp: {e}");
                ExitCode::FAILURE
            }
        };
    }
    plancraft_core::lock::set_app_name(format!("PlanCraft MCP server {}", env!("CARGO_PKG_VERSION")));
    let ws = match workspace_from_files(&cli.files) {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("plancraft-mcp: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Lock files of the open plans stay fresh while the server runs, and are given back when it
    // ends (the workspace is dropped with the server, or released below).
    plancraft_core::lock::spawn_keeper(Arc::downgrade(&ws), || {});
    let release = {
        let ws = Arc::downgrade(&ws);
        move || {
            if let Some(ws) = ws.upgrade() {
                ws.lock().unwrap_or_else(std::sync::PoisonError::into_inner).release_locks();
            }
        }
    };
    let options = ServerOptions { allow_file_access: cli.file_access, read_only: cli.read_only, ..ServerOptions::default() };
    let server = McpServer::with_options(ws, options);
    match cli.http {
        None => {
            eprintln!("plancraft-mcp {}: serving MCP on stdio ({} document(s) open)", env!("CARGO_PKG_VERSION"), cli.files.len());
            let result = run_stdio(&server);
            release();
            if let Err(e) = result {
                eprintln!("plancraft-mcp: stdio error: {e}");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Some(addr) => {
            let server = Arc::new(server);
            let token = crate::session::new_token();
            match serve_http(server, &addr, &token) {
                Ok(handle) => {
                    crate::session::remove_stale_http_tokens();
                    // The token goes to an owner-only file, never to the terminal or a log.
                    let token_file = match crate::session::publish_http_token(handle.addr(), handle.token()) {
                        Ok(f) => f,
                        Err(e) => {
                            eprintln!("plancraft-mcp: cannot write the bearer token file: {e}");
                            return ExitCode::FAILURE;
                        }
                    };
                    eprintln!("plancraft-mcp {}: serving MCP at {}", env!("CARGO_PKG_VERSION"), handle.url());
                    eprintln!("this session's bearer token is in {} (readable only by you)", token_file.display());
                    // The path isn't pasted into a shell command: a quote in it would break (or
                    // change) the command.
                    eprintln!(
                        "connect with: claude mcp add --transport http plancraft {} --header \"Authorization: Bearer <the token in that file>\"",
                        handle.url()
                    );
                    while handle.is_running() {
                        std::thread::sleep(std::time::Duration::from_millis(500));
                    }
                    crate::session::withdraw_http_token(&token_file, handle.token());
                    release();
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("plancraft-mcp: {e}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<Cli>, String> {
        parse_args(args.iter().map(|a| a.to_string()))
    }

    #[test]
    fn read_only_is_off_unless_asked() {
        assert!(!parse(&["plan.plancraft"]).unwrap().unwrap().read_only);
        let cli = parse(&["plan.plancraft", "--read-only", "--no-file-access"]).unwrap().unwrap();
        assert!(cli.read_only && !cli.file_access);
        assert_eq!(cli.files, ["plan.plancraft"]);
        assert!(USAGE.contains("--read-only"));
    }
}
