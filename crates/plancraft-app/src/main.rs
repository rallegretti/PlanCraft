//! PlanCraft — project management in Rust.
//!
//! ```text
//! plancraft [FILE.plancraft]          open the desktop app
//! plancraft mcp [FILE...] [--http ADDR]  run the MCP server without a window
//! ```

// Release builds on Windows are GUI applications (no console window). Command-line
// modes reattach to the console they were started from; see `attach_parent_console`.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod columns;
mod commands;
mod dialogs;
mod edit;
mod format;
mod logo;
mod mcp_host;
mod menubar;
mod recovery;
mod settings;
mod start_page;
mod state;
mod theme;
mod views;
mod widgets;

use plancraft_core::workspace::Workspace;

/// On Windows, a GUI-subsystem process has no console. When started from a terminal
/// for a command-line mode, attach to that terminal so output is visible. Inherited
/// pipes (e.g. an MCP client talking to `plancraft mcp`) already work and are left alone.
#[cfg(windows)]
#[allow(unsafe_code)]
fn attach_parent_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_OUTPUT_HANDLE};
    // SAFETY: `GetStdHandle` and `AttachConsole` take plain integer arguments and no pointers, have
    // no preconditions, and only report failure through their return values (a failure just means
    // there is no console to attach to). The returned handle is only compared, never dereferenced.
    unsafe {
        let out = GetStdHandle(STD_OUTPUT_HANDLE);
        if out.is_null() || out == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

/// Graphics backend selection. On Windows we use Direct3D 12 by default: it is always
/// present on Windows 10+, while some Vulkan drivers (e.g. older Intel HD Graphics)
/// crash during surface creation. `WGPU_BACKEND` still overrides the choice.
fn wgpu_options() -> eframe::WgpuConfiguration {
    #[allow(unused_mut)]
    let mut config = eframe::WgpuConfiguration::default();
    #[cfg(windows)]
    if std::env::var_os("WGPU_BACKEND").is_none()
        && let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut config.wgpu_setup
    {
        setup.instance_descriptor.backends = eframe::wgpu::Backends::DX12;
    }
    config
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(windows)]
    if matches!(args.first().map(String::as_str), Some("mcp" | "--version" | "--write-icon")) {
        attach_parent_console();
    }
    if args.first().map(String::as_str) == Some("mcp") {
        // Headless MCP server: `plancraft mcp [FILE...] [--http ADDR]`.
        let code = plancraft_mcp::cli::run(args.iter().skip(1).cloned().collect());
        std::process::exit(if code == std::process::ExitCode::SUCCESS { 0 } else { 1 });
    }
    if args.first().map(String::as_str) == Some("--version") {
        println!("plancraft {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--write-icon") {
        // Writes the rasterized logo as a PNG: `--write-icon [PATH] [SIZE]` (used by packaging).
        let size: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(512).clamp(16, 2048);
        let img =
            image::RgbaImage::from_raw(size, size, logo::rasterize(size)).ok_or_else(|| anyhow::anyhow!("icon buffer size mismatch"))?;
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
        wgpu_options: wgpu_options(),
        ..Default::default()
    };
    eframe::run_native("PlanCraft", options, Box::new(move |cc| Ok(Box::new(app::PlanCraftApp::new(cc, shared)))))
        .map_err(|e| anyhow::anyhow!("{e}"))
}
