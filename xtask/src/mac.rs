//! macOS packaging: universal `PlanCraft.app` inside a `.dmg`.
//!
//! Uses the system tools that ship with Xcode / macOS (`lipo`, `codesign`,
//! `hdiutil`, `notarytool`, `stapler`, `spctl`); everything else is done here.
//!
//! Releases are signed with the "Developer ID Application" certificate in the Mac's keychain
//! (hardened runtime, secure timestamp) and notarized by Apple: the app first, so its ticket
//! can be stapled to it, then the disk image. `notarytool` uses a keychain profile, `plancraft`
//! unless `PLANCRAFT_NOTARY_PROFILE` names another (made once with
//! `xcrun notarytool store-credentials plancraft`). `PLANCRAFT_SIGN_IDENTITY` picks the
//! certificate when the keychain has several. Credentials stay in the keychain; over SSH it must
//! be unlocked first (`security unlock-keychain`). `PLANCRAFT_ADHOC_SIGN=1` makes an ad-hoc
//! signed test build instead, which Gatekeeper warns about.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

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
    let signing = signing(&root)?;

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

    match &signing {
        Signing::DeveloperId { identity, profile } => {
            step("signing PlanCraft.app with the Developer ID (hardened runtime)");
            run(codesign(identity).args(["--identifier", "dev.plancraft.plancraft-mcp"]).arg(contents.join("MacOS/plancraft-mcp")))?;
            run(codesign(identity).arg(&app))?;
            run(Command::new("codesign").args(["--verify", "--deep", "--strict", "--verbose=2"]).arg(&app))?;
            step("notarizing PlanCraft.app (Apple's notary service; usually a few minutes)");
            let upload = stage.join("PlanCraft-notarize.zip");
            run(Command::new("ditto").args(["-c", "-k", "--keepParent"]).arg(&app).arg(&upload))?;
            let notarized = notarize(&upload, profile);
            let _ = std::fs::remove_file(&upload);
            notarized?;
            run(Command::new("xcrun").args(["stapler", "staple"]).arg(&app))?;
        }
        Signing::AdHoc => {
            step("ad-hoc signing (PLANCRAFT_ADHOC_SIGN: a test build; Gatekeeper will warn)");
            run(Command::new("codesign").args(["--force", "--deep", "--sign", "-"]).arg(&app))?;
            run(Command::new("codesign").args(["--verify", "--deep", "--strict"]).arg(&app))?;
        }
    }

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

    if let Signing::DeveloperId { identity, profile } = &signing {
        step("signing and notarizing the disk image");
        run(Command::new("codesign").args(["--force", "--timestamp", "--sign", identity]).arg(&dmg))?;
        notarize(&dmg, profile)?;
        run(Command::new("xcrun").args(["stapler", "staple"]).arg(&dmg))?;
        step("checking both with Gatekeeper");
        run(Command::new("spctl").args(["--assess", "--type", "open", "--context", "context:primary-signature", "--verbose=2"]).arg(&dmg))?;
        run(Command::new("spctl").args(["--assess", "--type", "execute", "--verbose=2"]).arg(&app))?;
    }

    let sum = util::write_checksum(&dmg)?;
    artifact(&dmg);
    artifact(&sum);
    Ok(vec![dmg, sum])
}

/// How the app is signed.
enum Signing {
    /// With a Developer ID certificate (its SHA-1 or name) and notarized with a notarytool profile.
    DeveloperId { identity: String, profile: String },
    /// Ad hoc, for a test build (`PLANCRAFT_ADHOC_SIGN=1`).
    AdHoc,
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// Works out how to sign, and checks the certificate and the notary profile can be used now,
/// before the long build (over SSH a locked keychain is the usual reason they can't).
fn signing(root: &Path) -> Result<Signing> {
    if env("PLANCRAFT_ADHOC_SIGN").is_some_and(|v| v != "0") {
        return Ok(Signing::AdHoc);
    }
    let identity = match env("PLANCRAFT_SIGN_IDENTITY") {
        Some(i) => i,
        None => {
            let found = output(Command::new("security").args(["find-identity", "-v", "-p", "codesigning"]))?;
            match developer_ids(&found).as_slice() {
                [(hash, name)] => {
                    step(format!("signing with {name}"));
                    hash.clone()
                }
                [] => bail!(
                    "no \"Developer ID Application\" certificate in this Mac's keychain, so the app can't be signed for \
                     release (set PLANCRAFT_ADHOC_SIGN=1 for an unsigned test build)"
                ),
                several => bail!(
                    "several Developer ID certificates; choose one with PLANCRAFT_SIGN_IDENTITY: {}",
                    several.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>().join(", ")
                ),
            }
        }
    };
    let profile = env("PLANCRAFT_NOTARY_PROFILE").unwrap_or_else(|| "plancraft".into());
    const UNLOCK: &str = "if you're signed in over SSH, unlock the keychain first: `security unlock-keychain`";

    step("checking the signing certificate can be used");
    let probe_dir = root.join("dist/mac-signcheck");
    util::fresh_dir(&probe_dir)?;
    let probe = probe_dir.join("probe");
    std::fs::copy("/usr/bin/true", &probe).context("copying a probe binary to sign")?;
    let (ok, text) = capture(Command::new("codesign").args(["--force", "--timestamp=none", "--sign", &identity]).arg(&probe))?;
    let _ = std::fs::remove_dir_all(&probe_dir);
    if !ok {
        bail!("can't sign with {identity}: {text}\n{UNLOCK}");
    }

    step(format!("checking the notary profile '{profile}'"));
    let (ok, text) = capture(Command::new("xcrun").args(["notarytool", "history", "--keychain-profile", &profile]))?;
    if !ok {
        let hint = if text.contains("keychainLocked") {
            UNLOCK.to_string()
        } else {
            format!(
                "make it with `xcrun notarytool store-credentials {profile}` (Apple ID, team ID and an app-specific \
                 password), or name another profile with PLANCRAFT_NOTARY_PROFILE"
            )
        };
        bail!("can't use the notary profile '{profile}': {text}\n{hint}");
    }
    Ok(Signing::DeveloperId { identity, profile })
}

/// `codesign` for distribution: hardened runtime (notarization requires it) and a secure timestamp.
fn codesign(identity: &str) -> Command {
    let mut c = Command::new("codesign");
    c.args(["--force", "--options", "runtime", "--timestamp", "--sign", identity]);
    c
}

/// Runs a command and returns whether it succeeded, with its stdout and stderr together.
fn capture(cmd: &mut Command) -> Result<(bool, String)> {
    let out = cmd.output().with_context(|| format!("failed to start {:?}", cmd.get_program()))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    Ok((out.status.success(), text.trim().to_string()))
}

/// Sends a zip or disk image to Apple's notary service and waits for the verdict; when Apple
/// refuses it, prints the notary log, which says why.
fn notarize(file: &Path, profile: &str) -> Result<()> {
    let (ok, text) =
        capture(Command::new("xcrun").args(["notarytool", "submit"]).arg(file).args(["--keychain-profile", profile, "--wait"]))?;
    for line in text.lines() {
        eprintln!("    {line}");
    }
    if ok && text.contains("status: Accepted") {
        return Ok(());
    }
    if let Some(id) = submission_id(&text) {
        let (_, log) = capture(Command::new("xcrun").args(["notarytool", "log", &id, "--keychain-profile", profile]))?;
        for line in log.lines() {
            eprintln!("    {line}");
        }
    }
    bail!("Apple did not notarize {}", file.display())
}

/// The submission id in `notarytool submit` output (`  id: 1234…`).
fn submission_id(text: &str) -> Option<String> {
    text.lines().find_map(|l| l.trim().strip_prefix("id: ")).map(|id| id.trim().to_string())
}

/// The Developer ID Application certificates (SHA-1, name) in `security find-identity` output.
fn developer_ids(text: &str) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        let Some((_, rest)) = line.split_once(") ") else { continue };
        let Some((hash, name)) = rest.split_once(' ') else { continue };
        let name = name.trim().trim_matches('"');
        if name.starts_with("Developer ID Application:") && !found.iter().any(|(h, _)| h == hash) {
            found.push((hash.to_string(), name.to_string()));
        }
    }
    found
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
    fn finds_developer_id_certificates() {
        let text = r#"  1) 65BFC9D781FBB580A8A0173A31431D95347443B8 "Developer ID Application: A Person (ABCDE12345)"
  2) 1111111111111111111111111111111111111111 "Apple Development: A Person (ZZZZZZZZZZ)"
     2 valid identities found"#;
        assert_eq!(
            super::developer_ids(text),
            vec![("65BFC9D781FBB580A8A0173A31431D95347443B8".to_string(), "Developer ID Application: A Person (ABCDE12345)".to_string())]
        );
        assert!(super::developer_ids("     0 valid identities found").is_empty());
    }

    #[test]
    fn reads_the_submission_id() {
        let text = "Conducting pre-submission checks for x.dmg...\nSubmission ID received\n  id: 2efe2717-52ef-43a5-96dc-0797e4ca1041\nProcessing complete\n  id: 2efe2717-52ef-43a5-96dc-0797e4ca1041\n  status: Invalid";
        assert_eq!(super::submission_id(text).as_deref(), Some("2efe2717-52ef-43a5-96dc-0797e4ca1041"));
        assert_eq!(super::submission_id("nothing"), None);
    }

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
