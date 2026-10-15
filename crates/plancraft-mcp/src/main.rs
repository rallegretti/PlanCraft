//! `plancraft-mcp` — standalone PlanCraft MCP server.

fn main() -> std::process::ExitCode {
    plancraft_mcp::cli::run(std::env::args().skip(1).collect())
}
