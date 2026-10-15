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
    plancraft-mcp [FILE...] [--http [ADDR]] [--no-file-access]

ARGS:
    FILE...             .plancraft documents to open (a missing file becomes a new,
                        unsaved project at that path). With no files the server starts
                        with no document; agents can call new_project / open_project.

OPTIONS:
    --http [ADDR]       Serve MCP Streamable HTTP on ADDR (default 127.0.0.1:7878)
                        at http://ADDR/mcp instead of stdio.
    --no-file-access    Refuse tools that open, create or link arbitrary file paths.
    -h, --help          Show this help.
    -V, --version       Show the version.

EXAMPLES:
    claude mcp add plancraft -- plancraft-mcp /path/to/project.plancraft
    plancraft-mcp project.plancraft --http 127.0.0.1:7878
    claude mcp add --transport http plancraft http://127.0.0.1:7878/mcp
";

struct Cli {
    files: Vec<String>,
    http: Option<String>,
    file_access: bool,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Option<Cli>, String> {
    let mut cli = Cli { files: Vec::new(), http: None, file_access: true };
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
                let addr = match args.peek() {
                    Some(next) if !next.starts_with('-') && !next.ends_with(".plancraft") && next.contains(':') => {
                        args.next().expect("peeked")
                    }
                    _ => DEFAULT_HTTP_ADDR.to_string(),
                };
                cli.http = Some(addr);
            }
            "--no-file-access" => cli.file_access = false,
            "--stdio" => cli.http = None,
            s if s.starts_with("--http=") => cli.http = Some(s["--http=".len()..].to_string()),
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
    let ws = match workspace_from_files(&cli.files) {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("plancraft-mcp: {e}");
            return ExitCode::FAILURE;
        }
    };
    let options = ServerOptions { allow_file_access: cli.file_access, ..ServerOptions::default() };
    let server = McpServer::with_options(ws, options);
    match cli.http {
        None => {
            eprintln!("plancraft-mcp {}: serving MCP on stdio ({} document(s) open)", env!("CARGO_PKG_VERSION"), cli.files.len());
            if let Err(e) = run_stdio(&server) {
                eprintln!("plancraft-mcp: stdio error: {e}");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Some(addr) => {
            let server = Arc::new(server);
            match serve_http(server, &addr) {
                Ok(handle) => {
                    eprintln!("plancraft-mcp {}: serving MCP at {}", env!("CARGO_PKG_VERSION"), handle.url());
                    eprintln!("connect with: claude mcp add --transport http plancraft {}", handle.url());
                    while handle.is_running() {
                        std::thread::sleep(std::time::Duration::from_millis(500));
                    }
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
