//! Linux packaging: a relocatable `.tar.gz` with an install script, desktop
//! entry, icons and a MIME type for `.plancraft` files.

use std::fs::File;
use std::path::PathBuf;

use anyhow::{Result, bail};
use flate2::Compression;
use flate2::write::GzEncoder;

use crate::util::{self, artifact, copy, fresh_dir, step};

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

    step("building release binaries");
    util::build_release(None, &[])?;

    let name = format!("plancraft-{version}-linux-{arch}");
    let stage = root.join("dist/linux").join(&name);
    fresh_dir(&stage)?;

    step("staging files");
    let release = root.join("target/release");
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
    Ok(vec![tarball, sum])
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
