//! Linux packaging: a relocatable `.tar.gz` with an install script, desktop entry, icons and a
//! MIME type for `.plancraft` files, and an AppImage (one file that runs without installing).
//!
//! The binaries are built in an Ubuntu 22.04 container (`linux-build.Dockerfile`), so they need
//! glibc 2.35 and run on Ubuntu 22.04 and newer; built on this machine they would need its own,
//! newer glibc. Set `PLANCRAFT_NATIVE_LINUX=1` to build here anyway (for a quick local package).
//!
//! Both packages carry the window-system libraries the app loads at startup that a fresh desktop
//! may lack (`BUNDLED_LIBS`, each in its own folder under `lib/plancraft/`); the app uses only
//! the ones the system lacks (`plancraft-app/src/bundled_libs.rs`). Before a package is finished,
//! it is started in a clean Ubuntu 22.04 container (`linux-smoke.Dockerfile`) and must open its
//! window there.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use flate2::Compression;
use flate2::write::GzEncoder;
use sha2::{Digest, Sha256};

use crate::util::{self, artifact, copy, fresh_dir, output, run, step};

/// A container image, made from its Dockerfile in `xtask/` the first time it is needed. Change the
/// tag when the Dockerfile changes, so existing machines make the new image.
struct Image {
    tag: &'static str,
    dockerfile: &'static str,
    about: &'static str,
}

/// The build image.
const BUILD_IMAGE: Image =
    Image { tag: "plancraft-build:ubuntu22.04-3", dockerfile: "linux-build.Dockerfile", about: "Ubuntu 22.04 with gcc and squashfs-tools" };

/// A clean desktop system (display server and Mesa only) for testing the packages.
const SMOKE_IMAGE: Image = Image {
    tag: "plancraft-smoke:ubuntu22.04",
    dockerfile: "linux-smoke.Dockerfile",
    about: "a clean Ubuntu 22.04 desktop: Xvfb and Mesa only",
};

/// Libraries winit and wgpu load at startup (keyboard maps, cursors, input devices, Wayland
/// surfaces) that a fresh desktop install may not have; copied from the build image, each
/// into its own folder `lib/plancraft/<name>/`, so the app can add just the ones a system lacks
/// (a library the system has is always used from the system). Not bundled, on purpose: glibc, libX11/libxcb, libwayland-client, and the
/// graphics drivers (libGL, libEGL, Vulkan, Mesa). Every desktop has them, and they must match
/// the system's own drivers and display server. The app trusts only these names (`KNOWN` in
/// `crates/plancraft-app/src/bundled_libs.rs`; keep the two lists in step).
const BUNDLED_LIBS: [&str; 7] = [
    "libxkbcommon.so.0",
    "libxkbcommon-x11.so.0",
    "libxcb-xkb.so.1",
    "libXcursor.so.1",
    "libXi.so.6",
    "libXrender.so.1",
    "libwayland-egl.so.1",
];

/// Starts the AppImage, the tarball's binary and a copy installed with `install.sh` in the clean
/// container: every bundled library must find what it needs there, and each must open its window
/// (it saves a screenshot and quits).
const SMOKE_TEST: &str = r#"
out=$1; image=$2; stage=$3
fail=0
bundled=$(printf '%s:' "$stage"/lib/plancraft/*); bundled=${bundled%:}
for lib in "$stage"/lib/plancraft/*/* "$stage"/bin/plancraft; do
  missing=$(LD_LIBRARY_PATH="$bundled" ldd "$lib" | grep 'not found')
  if [ -n "$missing" ]; then echo "$lib needs a library a clean system lacks:"; echo "$missing"; fail=1; fi
done
start() {
  name=$1; shift
  rm -f "$out/$name.png"
  PLANCRAFT_SCREENSHOT="$out/$name.png" APPIMAGE_EXTRACT_AND_RUN=1     timeout 180 xvfb-run -a -s "-screen 0 1440x900x24" "$@" --demo >"$out/$name.log" 2>&1
  if [ -s "$out/$name.png" ]; then
    echo "    $name: opened its window"
  else
    echo "$name did not open its window on a clean system:"; cat "$out/$name.log"; fail=1
  fi
}
start appimage "$image"
start tarball "$stage/bin/plancraft"
prefix=$(mktemp -d)/prefix
if "$stage/install.sh" "$prefix" >"$out/install.log" 2>&1; then
  start installed "$prefix/bin/plancraft"
else
  echo "install.sh failed:"; cat "$out/install.log"; fail=1
fi
exit $fail
"#;

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
# The libraries' folder belongs to PlanCraft alone: replaced whole on install (so none from an
# older version stays behind), removed whole on uninstall.
rm -rf "$prefix/lib/plancraft"

files="bin/plancraft bin/plancraft-mcp share/applications/plancraft.desktop share/mime/packages/plancraft.xml"
for icon in "$here"/share/icons/hicolor/*/apps/plancraft.*; do
  files="$files ${icon#"$here"/}"
done
# Libraries the app loads at startup that some systems lack; only PlanCraft looks in this
# folder, and only for the ones the system does not have.
for lib in "$here"/lib/plancraft/*/*; do
  files="$files ${lib#"$here"/}"
done

for f in $files; do
  if [ "$uninstall" = 1 ]; then
    rm -f "$prefix/$f"
  else
    mkdir -p "$(dirname "$prefix/$f")"
    cp "$here/$f" "$prefix/$f"
  fi
done
if [ "$uninstall" = 0 ]; then
  chmod 755 "$prefix/bin/plancraft" "$prefix/bin/plancraft-mcp"
  # Readable by everyone, writable only by the owner (whatever the umask): PlanCraft trusts the
  # libraries only then.
  find "$prefix/lib/plancraft" -type d -exec chmod 755 {} +
  find "$prefix/lib/plancraft" -type f -exec chmod 644 {} +
fi

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
    bundle_libs(&root, &stage.join("lib/plancraft"))?;
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
    smoke_test(&root, &stage, &image)?;
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
    step(format!("building release binaries in an Ubuntu 22.04 container ({})", BUILD_IMAGE.tag));
    let target = root.join("target/ubuntu22.04");
    let cargo = PathBuf::from(output(Command::new("rustup").args(["which", "cargo"]))?.trim());
    let bin = cargo.parent().context("finding the toolchain")?.to_path_buf();
    let toolchain = bin.parent().context("finding the toolchain")?.to_path_buf();
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")))
        .context("finding CARGO_HOME")?;
    let mut c = container(root, &BUILD_IMAGE)?;
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
        .arg(BUILD_IMAGE.tag)
        .arg(&cargo)
        .args(["build", "--release", "--locked", "-p", "plancraft-app", "-p", "plancraft-mcp"]);
    run(&mut c)?;
    Ok(target.join("release"))
}

/// `docker run` as this user, with the repository mounted at its own path; the caller adds the
/// image's tag and the command (the image is made the first time it is needed).
fn container(root: &Path, image: &Image) -> Result<Command> {
    if output(Command::new("docker").args(["image", "inspect", image.tag])).is_err() {
        step(format!("making the container image {} ({})", image.tag, image.about));
        run(Command::new("docker")
            .args(["build", "-t", image.tag, "-f"])
            .arg(root.join("xtask").join(image.dockerfile))
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
    copy_tree(&stage.join("lib"), &appdir.join("usr/lib"))?;
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
    let mut c = container(root, &BUILD_IMAGE)?;
    c.args([BUILD_IMAGE.tag, "mksquashfs"]).arg(&appdir).arg(&squashfs).args(["-root-owned", "-noappend", "-comp", "zstd", "-quiet"]);
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

/// The folder a bundled library goes in: its soname without the version (`libXi.so.6` → `libXi`).
fn lib_dir(soname: &str) -> &str {
    soname.split(".so").next().unwrap_or(soname)
}

/// Copies `BUNDLED_LIBS` out of the build image, each into `dest/<name>/` under its soname
/// (symlinks resolved).
fn bundle_libs(root: &Path, dest: &Path) -> Result<()> {
    step("bundling the window-system libraries");
    fresh_dir(dest)?;
    for lib in BUNDLED_LIBS {
        std::fs::create_dir_all(dest.join(lib_dir(lib)))?;
    }
    let script = r#"dest=$1; shift; for l in "$@"; do cp -L "/usr/lib/x86_64-linux-gnu/$l" "$dest/${l%%.so*}/" || exit 1; done"#;
    let mut c = container(root, &BUILD_IMAGE)?;
    c.args([BUILD_IMAGE.tag, "sh", "-c", script, "sh"]).arg(dest).args(BUNDLED_LIBS);
    run(&mut c)?;
    for lib in BUNDLED_LIBS {
        let path = dest.join(lib_dir(lib)).join(lib);
        if !path.is_file() {
            bail!("{lib} was not bundled");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
        }
    }
    Ok(())
}

/// Starts both packages on a clean system (see `SMOKE_TEST`); fails the build if either does not
/// open its window. `PLANCRAFT_SKIP_SMOKE_TEST=1` skips it (never for a release).
fn smoke_test(root: &Path, stage: &Path, image: &Path) -> Result<()> {
    if std::env::var_os("PLANCRAFT_SKIP_SMOKE_TEST").is_some() {
        step("SKIPPING the clean-system test (PLANCRAFT_SKIP_SMOKE_TEST): do not release these packages");
        return Ok(());
    }
    if std::env::var_os("PLANCRAFT_NATIVE_LINUX").is_some() {
        step("skipping the clean-system test: PLANCRAFT_NATIVE_LINUX binaries need this machine's newer glibc");
        return Ok(());
    }
    step("starting both packages on a clean Ubuntu 22.04 desktop");
    let out = root.join("dist/linux/smoke");
    fresh_dir(&out)?;
    let mut c = container(root, &SMOKE_IMAGE)?;
    c.args([SMOKE_IMAGE.tag, "sh", "-c", SMOKE_TEST, "sh"]).arg(&out).arg(image).arg(stage);
    run(&mut c).context("a Linux package does not start on a clean system (the log is above; screenshots and logs in dist/linux/smoke)")
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
