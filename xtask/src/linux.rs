//! Linux packaging: a relocatable `.tar.gz` with an install script, desktop entry, icons and a
//! MIME type for `.plancraft` files, and an AppImage (one file that runs without installing).
//!
//! The binaries are built in an Ubuntu 22.04 container (`linux-build.Dockerfile`), so they need
//! glibc 2.35 and run on Ubuntu 22.04 and newer; built on this machine they would need its own,
//! newer glibc. Set `PLANCRAFT_NATIVE_LINUX=1` to build here anyway (for a quick local package).

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use flate2::Compression;
use flate2::write::GzEncoder;
use sha2::{Digest, Sha256};

use crate::util::{self, artifact, copy, fresh_dir, output, run, step};

/// The build image (made from `xtask/linux-build.Dockerfile` the first time it is needed).
const BUILD_IMAGE: &str = "plancraft-build:ubuntu22.04";

/// The AppImage runtime (the small launcher at the start of every AppImage), from the AppImage
/// project's own releases, pinned by the SHA-256 GitHub publishes for it.
const RUNTIME_URL: &str = "https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64";
const RUNTIME_SHA256: &str = "2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d";

const APPRUN: &str = r#"#!/bin/sh
# Starts PlanCraft from inside the AppImage; arguments (a file to open, `mcp --attach`) pass on.
here=$(dirname "$(readlink -f "$0")")
exec "$here/usr/bin/plancraft" "$@"
"#;

const ICON_SIZES: [u32; 6] = [32, 48, 64, 128, 256, 512];

const DESKTOP_ENTRY: &str = "[Desktop Entry]
Type=Application
Name=PlanCraft
GenericName=Project Planner
Comment=Plan projects with tasks, milestones, resources and Gantt charts
Exec=plancraft %f
Icon=plancraft
Terminal=false
Categories=Office;ProjectManagement;
MimeType=application/x-plancraft;
Keywords=project;gantt;schedule;planning;tasks;milestones;
StartupWMClass=plancraft
";

const MIME_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<mime-info xmlns="http://www.freedesktop.org/standards/shared-mime-info">
  <mime-type type="application/x-plancraft">
    <comment>PlanCraft project</comment>
    <glob pattern="*.plancraft"/>
    <icon name="plancraft"/>
  </mime-type>
</mime-info>
"#;

const INSTALL_SH: &str = r#"#!/bin/sh
# Installs PlanCraft for the current user (default) or system-wide:
#   ./install.sh               -> ~/.local
#   sudo ./install.sh /usr/local
#   ./install.sh --uninstall [PREFIX]
set -eu
here=$(cd "$(dirname "$0")" && pwd)
uninstall=0
if [ "${1:-}" = "--uninstall" ]; then uninstall=1; shift; fi
prefix=${1:-"$HOME/.local"}

files="bin/plancraft bin/plancraft-mcp share/applications/plancraft.desktop share/mime/packages/plancraft.xml"
for icon in "$here"/share/icons/hicolor/*/apps/plancraft.*; do
  files="$files ${icon#"$here"/}"
done

for f in $files; do
  if [ "$uninstall" = 1 ]; then
    rm -f "$prefix/$f"
  else
    mkdir -p "$(dirname "$prefix/$f")"
    cp "$here/$f" "$prefix/$f"
  fi
done
[ "$uninstall" = 1 ] || chmod 755 "$prefix/bin/plancraft" "$prefix/bin/plancraft-mcp"

command -v update-mime-database >/dev/null 2>&1 && update-mime-database "$prefix/share/mime" >/dev/null 2>&1 || true
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$prefix/share/applications" >/dev/null 2>&1 || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t "$prefix/share/icons/hicolor" >/dev/null 2>&1 || true

if [ "$uninstall" = 1 ]; then
  echo "PlanCraft removed from $prefix"
else
  echo "PlanCraft installed to $prefix"
  case ":$PATH:" in *":$prefix/bin:"*) ;; *) echo "Note: add $prefix/bin to your PATH to run 'plancraft' from a terminal." ;; esac
fi
"#;

pub fn package() -> Result<Vec<PathBuf>> {
    if !cfg!(target_os = "linux") {
        bail!("Linux packages must be built on Linux");
    }
    let root = util::root();
    let version = util::version()?;
    let arch = std::env::consts::ARCH;

    let release = build_binaries(&root)?;

    let name = format!("plancraft-{version}-linux-{arch}");
    let stage = root.join("dist/linux").join(&name);
    fresh_dir(&stage)?;

    step("staging files");
    copy(&release.join("plancraft"), &stage.join("bin/plancraft"), true)?;
    copy(&release.join("plancraft-mcp"), &stage.join("bin/plancraft-mcp"), true)?;
    copy(&root.join("README.md"), &stage.join("README.md"), false)?;
    copy(&root.join("LICENSE"), &stage.join("LICENSE"), false)?;
    copy(&root.join("assets/logo.svg"), &stage.join("share/icons/hicolor/scalable/apps/plancraft.svg"), false)?;
    for size in ICON_SIZES {
        let out = stage.join(format!("share/icons/hicolor/{size}x{size}/apps/plancraft.png"));
        util::render_icon(&stage.join("bin/plancraft"), &out, size)?;
    }
    write(&stage.join("share/applications/plancraft.desktop"), DESKTOP_ENTRY, false)?;
    write(&stage.join("share/mime/packages/plancraft.xml"), MIME_XML, false)?;
    write(&stage.join("install.sh"), INSTALL_SH, true)?;

    step("creating the tarball");
    let tarball = root.join(format!("dist/{name}.tar.gz"));
    let encoder = GzEncoder::new(File::create(&tarball)?, Compression::best());
    let mut tar = tar::Builder::new(encoder);
    tar.mode(tar::HeaderMode::Deterministic);
    tar.follow_symlinks(false);
    tar.append_dir_all(&name, &stage)?;
    tar.into_inner()?.finish()?;

    let sum = util::write_checksum(&tarball)?;
    artifact(&tarball);
    artifact(&sum);

    let (image, image_sum) = appimage(&root, &stage, &version, arch)?;
    Ok(vec![tarball, sum, image, image_sum])
}

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Builds the release binaries; returns the directory they are in.
fn build_binaries(root: &Path) -> Result<PathBuf> {
    if std::env::var_os("PLANCRAFT_NATIVE_LINUX").is_some() {
        step("building release binaries on this machine (PLANCRAFT_NATIVE_LINUX: they need this machine's glibc or newer)");
        util::build_release(None, &[])?;
        return Ok(root.join("target/release"));
    }
    step(format!("building release binaries in an Ubuntu 22.04 container ({BUILD_IMAGE})"));
    let target = root.join("target/ubuntu22.04");
    let cargo = PathBuf::from(output(Command::new("rustup").args(["which", "cargo"]))?.trim());
    let bin = cargo.parent().context("finding the toolchain")?.to_path_buf();
    let toolchain = bin.parent().context("finding the toolchain")?.to_path_buf();
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")))
        .context("finding CARGO_HOME")?;
    let mut c = container(root)?;
    c.arg("-e")
        .arg(format!("CARGO_HOME={}", cargo_home.display()))
        .arg("-e")
        .arg(format!("CARGO_TARGET_DIR={}", target.display()))
        .arg("-e")
        .arg(format!("RUSTC={}", bin.join("rustc").display()))
        .arg("-e")
        .arg(format!("PATH={}:/usr/bin:/bin", bin.display()))
        .arg("-v")
        .arg(format!("{0}:{0}", cargo_home.display()))
        .arg("-v")
        .arg(format!("{0}:{0}:ro", toolchain.display()))
        .arg(BUILD_IMAGE)
        .arg(&cargo)
        .args(["build", "--release", "--locked", "-p", "plancraft-app", "-p", "plancraft-mcp"]);
    run(&mut c)?;
    Ok(target.join("release"))
}

/// `docker run` in the build image as this user, with the repository mounted at its own path
/// (the image is made the first time it is needed).
fn container(root: &Path) -> Result<Command> {
    if output(Command::new("docker").args(["image", "inspect", BUILD_IMAGE])).is_err() {
        step(format!("making the build image {BUILD_IMAGE} (Ubuntu 22.04 with gcc and squashfs-tools)"));
        run(Command::new("docker")
            .args(["build", "-t", BUILD_IMAGE, "-f"])
            .arg(root.join("xtask/linux-build.Dockerfile"))
            .arg(root.join("xtask")))?;
    }
    let uid = output(Command::new("id").arg("-u"))?;
    let gid = output(Command::new("id").arg("-g"))?;
    let mut c = Command::new("docker");
    c.args(["run", "--rm", "-u"])
        .arg(format!("{}:{}", uid.trim(), gid.trim()))
        .args(["-e", "HOME=/tmp", "-v"])
        .arg(format!("{0}:{0}", root.display()))
        .arg("-w")
        .arg(root);
    Ok(c)
}

/// The AppImage runtime, downloaded once into `target/appimage` and checked against its SHA-256.
fn runtime(root: &Path) -> Result<Vec<u8>> {
    let path = root.join("target/appimage/runtime-x86_64-20251108");
    if !path.exists() {
        step("downloading the AppImage runtime");
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let part = path.with_extension("part");
        run(Command::new("curl").args(["-fsSL", "--proto", "=https", "-o"]).arg(&part).arg(RUNTIME_URL))?;
        std::fs::rename(&part, &path)?;
    }
    let bytes = std::fs::read(&path)?;
    if hex(&bytes) != RUNTIME_SHA256 {
        let _ = std::fs::remove_file(&path);
        bail!("the AppImage runtime's checksum is wrong; it was removed (run the command again to download it afresh)");
    }
    Ok(bytes)
}

/// Builds `PlanCraft-<version>-<arch>.AppImage` from the staged tarball files.
fn appimage(root: &Path, stage: &Path, version: &str, arch: &str) -> Result<(PathBuf, PathBuf)> {
    if arch != "x86_64" {
        bail!("the AppImage runtime pinned here is for x86_64");
    }
    let runtime = runtime(root)?;
    step("staging the AppImage");
    let appdir = root.join("dist/linux/PlanCraft.AppDir");
    fresh_dir(&appdir)?;
    for bin in ["plancraft", "plancraft-mcp"] {
        copy(&stage.join("bin").join(bin), &appdir.join("usr/bin").join(bin), true)?;
    }
    copy_tree(&stage.join("share"), &appdir.join("usr/share"))?;
    write(&appdir.join("AppRun"), APPRUN, true)?;
    let desktop = format!("{DESKTOP_ENTRY}X-AppImage-Version={version}\n");
    write(&appdir.join("plancraft.desktop"), &desktop, false)?;
    let icon = stage.join("share/icons/hicolor/256x256/apps/plancraft.png");
    copy(&icon, &appdir.join("plancraft.png"), false)?;
    copy(&icon, &appdir.join(".DirIcon"), false)?;

    step("packing the AppImage");
    let squashfs = root.join("dist/linux/PlanCraft.squashfs");
    let _ = std::fs::remove_file(&squashfs);
    let mut c = container(root)?;
    c.args([BUILD_IMAGE, "mksquashfs"]).arg(&appdir).arg(&squashfs).args(["-root-owned", "-noappend", "-comp", "zstd", "-quiet"]);
    run(&mut c)?;
    let image = root.join(format!("dist/PlanCraft-{version}-{arch}.AppImage"));
    let mut bytes = runtime;
    bytes.extend(std::fs::read(&squashfs)?);
    std::fs::write(&image, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&image, std::fs::Permissions::from_mode(0o755))?;
    }
    let sum = util::write_checksum(&image)?;
    artifact(&image);
    artifact(&sum);
    Ok((image, sum))
}

/// Copies a directory tree (regular files only).
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    for entry in std::fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
        let entry = entry?;
        let path = entry.path();
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&path, &dest)?;
        } else {
            copy(&path, &dest, false)?;
        }
    }
    Ok(())
}

fn write(path: &std::path::Path, text: &str, executable: bool) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }))?;
    }
    let _ = executable;
    Ok(())
}
