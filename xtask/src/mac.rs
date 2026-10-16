//! macOS packaging: universal `PlanCraft.app` inside a `.dmg`.
//!
//! Uses the system tools that ship with Xcode / macOS (`lipo`, `codesign`,
//! `hdiutil`); everything else is done here. Signing is ad-hoc for now: the app
//! runs, but Gatekeeper asks users to confirm on first launch.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, bail};

use crate::util::{self, artifact, copy, fresh_dir, output, run, step};

const TARGETS: [&str; 2] = ["aarch64-apple-darwin", "x86_64-apple-darwin"];
const BUNDLE_ID: &str = "dev.plancraft.PlanCraft";
const MIN_MACOS: &str = "11.0";

/// Icon slots in an `.icns` file and their pixel sizes (all stored as PNG).
const ICNS_SLOTS: [(&[u8; 4], u32); 10] = [
    (b"icp4", 16),
    (b"icp5", 32),
    (b"icp6", 64),
    (b"ic07", 128),
    (b"ic08", 256),
    (b"ic09", 512),
    (b"ic10", 1024),
    (b"ic11", 32),
    (b"ic13", 256),
    (b"ic14", 512),
];

pub fn package() -> Result<Vec<PathBuf>> {
    if !cfg!(target_os = "macos") {
        bail!("macOS packages must be built on a Mac (use `cargo xtask dist mac` to build on a remote Mac)");
    }
    let root = util::root();
    let version = util::version()?;

    step("checking Rust targets");
    let installed = output(Command::new("rustup").args(["target", "list", "--installed"]))?;
    let missing: Vec<&str> = TARGETS.iter().copied().filter(|t| !installed.lines().any(|l| l.trim() == *t)).collect();
    if !missing.is_empty() {
        bail!("missing Rust targets; install them with: rustup target add {}", missing.join(" "));
    }

    for t in TARGETS {
        step(format!("building release binaries for {t}"));
        util::build_release(Some(t), &[("MACOSX_DEPLOYMENT_TARGET", MIN_MACOS)])?;
    }

    let stage = root.join("dist/mac");
    fresh_dir(&stage)?;
    let bins = stage.join("bin");
    std::fs::create_dir_all(&bins)?;
    for name in ["plancraft", "plancraft-mcp"] {
        step(format!("creating universal {name}"));
        let mut lipo = Command::new("lipo");
        lipo.arg("-create").arg("-output").arg(bins.join(name));
        for t in TARGETS {
            lipo.arg(root.join(format!("target/{t}/release/{name}")));
        }
        run(&mut lipo)?;
    }

    step("rendering the app icon");
    let host_target = if cfg!(target_arch = "aarch64") { TARGETS[0] } else { TARGETS[1] };
    let host_app = root.join(format!("target/{host_target}/release/plancraft"));
    let icns = build_icns(&host_app, &stage.join("icon"))?;

    step("assembling PlanCraft.app");
    let dmg_root = stage.join("dmg");
    let app = dmg_root.join("PlanCraft.app");
    let contents = app.join("Contents");
    copy(&bins.join("plancraft"), &contents.join("MacOS/PlanCraft"), true)?;
    copy(&bins.join("plancraft-mcp"), &contents.join("MacOS/plancraft-mcp"), true)?;
    std::fs::create_dir_all(contents.join("Resources"))?;
    std::fs::write(contents.join("Resources/PlanCraft.icns"), icns)?;
    std::fs::write(contents.join("Info.plist"), info_plist(&version))?;
    std::fs::write(contents.join("PkgInfo"), "APPL????")?;

    step("ad-hoc signing (unsigned for distribution)");
    run(Command::new("codesign").args(["--force", "--deep", "--sign", "-"]).arg(&app))?;
    run(Command::new("codesign").args(["--verify", "--deep", "--strict"]).arg(&app))?;

    step("creating the disk image");
    #[cfg(unix)]
    anyhow::Context::context(std::os::unix::fs::symlink("/Applications", dmg_root.join("Applications")), "creating Applications link")?;
    let dmg = root.join(format!("dist/PlanCraft-{version}-macos-universal.dmg"));
    if dmg.exists() {
        std::fs::remove_file(&dmg)?;
    }
    run(Command::new("hdiutil")
        .args(["create", "-volname", &format!("PlanCraft {version}"), "-fs", "HFS+", "-format", "UDZO", "-ov", "-srcfolder"])
        .arg(&dmg_root)
        .arg(&dmg))?;

    let sum = util::write_checksum(&dmg)?;
    artifact(&dmg);
    artifact(&sum);
    Ok(vec![dmg, sum])
}

/// Renders every icon size with the app binary and packs them into an `.icns` file.
fn build_icns(app_binary: &Path, dir: &Path) -> Result<Vec<u8>> {
    std::fs::create_dir_all(dir)?;
    let mut entries: Vec<(&[u8; 4], Vec<u8>)> = Vec::new();
    for (code, size) in ICNS_SLOTS {
        let png = dir.join(format!("icon_{size}.png"));
        if !png.exists() {
            util::render_icon(app_binary, &png, size)?;
        }
        entries.push((code, std::fs::read(&png)?));
    }
    Ok(icns(&entries))
}

/// Encodes an Apple Icon Image file: `icns` header followed by typed chunks.
fn icns(entries: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let body_len: usize = entries.iter().map(|(_, data)| 8 + data.len()).sum();
    let mut out = Vec::with_capacity(8 + body_len);
    out.extend_from_slice(b"icns");
    out.extend_from_slice(&((8 + body_len) as u32).to_be_bytes());
    for (code, data) in entries {
        out.extend_from_slice(*code);
        out.extend_from_slice(&((8 + data.len()) as u32).to_be_bytes());
        out.extend_from_slice(data);
    }
    out
}

fn info_plist(version: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key><string>en</string>
    <key>CFBundleDisplayName</key><string>PlanCraft</string>
    <key>CFBundleExecutable</key><string>PlanCraft</string>
    <key>CFBundleIconFile</key><string>PlanCraft</string>
    <key>CFBundleIdentifier</key><string>{BUNDLE_ID}</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>CFBundleName</key><string>PlanCraft</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>{version}</string>
    <key>CFBundleVersion</key><string>{version}</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
    <key>LSMinimumSystemVersion</key><string>{MIN_MACOS}</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSHumanReadableCopyright</key><string>Dedicated to the public domain (CC0 1.0)</string>
</dict>
</plist>
"#
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn icns_layout() {
        let data = super::icns(&[(b"ic07", vec![1, 2, 3])]);
        assert_eq!(&data[0..4], b"icns");
        assert_eq!(u32::from_be_bytes(data[4..8].try_into().unwrap()), 19);
        assert_eq!(&data[8..12], b"ic07");
        assert_eq!(u32::from_be_bytes(data[12..16].try_into().unwrap()), 11);
        assert_eq!(&data[16..], &[1, 2, 3]);
    }
}
