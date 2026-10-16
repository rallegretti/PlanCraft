//! PlanCraft — project management in Rust.
//!
//! ```text
//! plancraft [FILE.plancraft]          open the desktop app
//! plancraft mcp [FILE...] [--http ADDR]  run the MCP server without a window
//! ```

mod app;
mod columns;
mod commands;
mod dialogs;
mod format;
mod logo;
mod mcp_host;
mod menubar;
mod settings;
mod start_page;
mod state;
mod theme;
mod views;
mod widgets;

use plancraft_core::workspace::Workspace;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("mcp") {
        // Headless MCP server: `plancraft mcp [FILE...] [--http ADDR]`.
        let code = plancraft_mcp::cli::run(args[1..].to_vec());
        std::process::exit(if code == std::process::ExitCode::SUCCESS { 0 } else { 1 });
    }
    if args.first().map(String::as_str) == Some("--version") {
        println!("plancraft {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--write-icon") {
        // Writes the rasterized logo as a PNG: `--write-icon [PATH] [SIZE]` (used by packaging).
        let size: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(512).clamp(16, 2048);
        let img = image::RgbaImage::from_raw(size, size, logo::rasterize(size)).expect("icon buffer");
        img.save(args.get(1).map(String::as_str).unwrap_or("plancraft-icon.png"))?;
        return Ok(());
    }

    let mut ws = Workspace::default();
    for a in &args {
        if a.starts_with('-') {
            continue;
        }
        if let Err(e) = ws.open(a) {
            eprintln!("plancraft: {e}");
        }
    }
    if args.iter().any(|a| a == "--demo") {
        let today = chrono::Local::now().date_naive();
        let monday = today - chrono::Duration::days(chrono::Datelike::weekday(&today).num_days_from_monday() as i64 + 14);
        ws.add(plancraft_core::sample::demo_project(monday), None);
    }
    let shared = ws.shared();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("PlanCraft")
            .with_app_id("plancraft")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([900.0, 560.0])
            .with_icon(logo::icon_data()),
        ..Default::default()
    };
    eframe::run_native("PlanCraft", options, Box::new(move |cc| Ok(Box::new(app::PlanCraftApp::new(cc, shared)))))
        .map_err(|e| anyhow::anyhow!("{e}"))
}
