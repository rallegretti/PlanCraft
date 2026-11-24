//! Linux packaging: a relocatable `.tar.gz` with an install script, desktop entry, icons and a
//! MIME type for `.plancraft` files, and an AppImage (one file that runs without installing),
//! each for x86_64 and for arm64 (aarch64).
//!
//! The binaries are built in an Ubuntu 22.04 container (`linux-build.Dockerfile`), so they need
//! glibc 2.35 and run on Ubuntu 22.04 and newer; built on this machine they would need its own,
//! newer glibc. The container builds x86_64 natively and cross-compiles arm64, so the build host
//! must be x86_64. Set `PLANCRAFT_NATIVE_LINUX=1` to build here anyway (for a quick local package
//! of this machine's architecture only).
//!
//! Both packages carry the window-system libraries the app loads at startup that a fresh desktop
//! may lack (`BUNDLED_LIBS`, each in its own folder under `lib/plancraft/`); the app uses only
//! the ones the system lacks (`plancraft-app/src/bundled_libs.rs`). Before a package is finished,
//! it is started in a clean Ubuntu 22.04 container (`linux-smoke.Dockerfile`) and must open its
//! window there; the arm64 packages run in an arm64 container under QEMU emulation, which the
//! build host must have registered (`binfmt_misc` with the `F` flag; see `QEMU_ADVICE`).

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
    /// Docker's name for the image's architecture (`linux/amd64`, `linux/arm64`).
    platform: &'static str,
    about: &'static str,
}

/// The build image (x86_64; cross-compiles arm64).
const BUILD_IMAGE: Image = Image {
    tag: "plancraft-build:ubuntu22.04-4",
    dockerfile: "linux-build.Dockerfile",
    platform: "linux/amd64",
    about: "Ubuntu 22.04 with gcc, the arm64 cross linker and squashfs-tools",
};

/// An architecture the Linux packages are made for.
struct Arch {
    /// The name in file names and `std::env::consts::ARCH`.
    name: &'static str,
    /// Rust's target triple.
    triple: &'static str,
    /// Where Ubuntu keeps this architecture's libraries (the bundled ones are copied from here).
    lib_dir: &'static str,
    /// The AppImage runtime for this architecture and the SHA-256 GitHub publishes for it.
    runtime: &'static str,
    runtime_sha256: &'static str,
    /// A clean desktop system (display server and Mesa only) for testing the packages.
    smoke_image: Image,
    /// The `binfmt_misc` entry that runs this architecture's programs on an x86_64 machine
    /// (`None`: runs natively).
    emulator: Option<&'static str>,
}

const ARCHES: [Arch; 2] = [
    Arch {
        name: "x86_64",
        triple: "x86_64-unknown-linux-gnu",
        lib_dir: "/usr/lib/x86_64-linux-gnu",
        runtime: "runtime-x86_64",
        runtime_sha256: "2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d",
        smoke_image: Image {
            tag: "plancraft-smoke:ubuntu22.04",
            dockerfile: "linux-smoke.Dockerfile",
            platform: "linux/amd64",
            about: "a clean Ubuntu 22.04 desktop: Xvfb and Mesa only",
        },
        emulator: None,
    },
    Arch {
        name: "aarch64",
        triple: "aarch64-unknown-linux-gnu",
        lib_dir: "/usr/lib/aarch64-linux-gnu",
        runtime: "runtime-aarch64",
        runtime_sha256: "00cbdfcf917cc6c0ff6d3347d59e0ca1f7f45a6df1a428a0d6d8a78664d87444",
        smoke_image: Image {
            tag: "plancraft-smoke:ubuntu22.04-arm64",
            dockerfile: "linux-smoke.Dockerfile",
            platform: "linux/arm64",
            about: "a clean arm64 Ubuntu 22.04 desktop: Xvfb and Mesa only (runs under QEMU)",
        },
        emulator: Some("qemu-aarch64"),
    },
];

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
/// (it saves a screenshot and quits). Under emulation the AppImage is started through the
/// emulator (`$5`): its header carries the AppImage mark in bytes the kernel's emulator entry
/// requires to be zero, so the kernel would not hand it to QEMU (real ARM hardware runs it
/// directly).
const SMOKE_TEST: &str = r#"
out=$1; image=$2; stage=$3; limit=$4; emulator=$5
fail=0
bundled=$(printf '%s:' "$stage"/lib/plancraft/*); bundled=${bundled%:}
for lib in "$stage"/lib/plancraft/*/* "$stage"/bin/plancraft "$stage"/bin/plancraft-server; do
  missing=$(LD_LIBRARY_PATH="$bundled" ldd "$lib" | grep 'not found')
  if [ -n "$missing" ]; then echo "$lib needs a library a clean system lacks:"; echo "$missing"; fail=1; fi
done
start() {
  name=$1; shift
  rm -f "$out/$name.png"
  PLANCRAFT_SCREENSHOT="$out/$name.png" APPIMAGE_EXTRACT_AND_RUN=1 timeout "$limit" xvfb-run -a -s "-screen 0 1440x900x24" "$@" --demo >"$out/$name.log" 2>&1
  if [ -s "$out/$name.png" ]; then
    echo "    $name: opened its window"
  else
    echo "$name did not open its window on a clean system:"; cat "$out/$name.log"; fail=1
  fi
}
start appimage ${emulator:+"$emulator"} "$image"
start tarball "$stage/bin/plancraft"
# PlanCraft Server alone runs with no desktop at all.
if ! ${emulator:+"$emulator"} "$stage/bin/plancraft-server" --version >"$out/server.log" 2>&1; then
  echo "plancraft-server did not run on a clean system:"; cat "$out/server.log"; fail=1
fi
prefix=$(mktemp -d)/prefix
if "$stage/install.sh" "$prefix" >"$out/install.log" 2>&1; then
  start installed "$prefix/bin/plancraft"
else
  echo "install.sh failed:"; cat "$out/install.log"; fail=1
fi
exit $fail
"#;

/// The AppImage runtime (the small launcher at the start of every AppImage), from the AppImage
/// project's own releases (`Arch::runtime`), pinned by the SHA-256 GitHub publishes for it.
const RUNTIME_RELEASE: &str = "https://github.com/AppImage/type2-runtime/releases/download/20251108";

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

files="bin/plancraft bin/plancraft-mcp bin/plancraft-server share/applications/plancraft.desktop share/mime/packages/plancraft.xml"
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
  chmod 755 "$prefix/bin/plancraft" "$prefix/bin/plancraft-mcp" "$prefix/bin/plancraft-server"
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

    // A missing emulator should stop the build now, not after the build and the packaging.
    if !smoke_test_skipped() && std::env::var_os("PLANCRAFT_NATIVE_LINUX").is_none() {
        for arch in &ARCHES {
            if let Some(entry) = arch.emulator {
                emulator(entry, arch)?;
            }
        }
    }
    let builds = build_binaries(&root)?;
    // The icons are drawn by the app itself, so by the binary this machine can run.
    let icons = root.join("dist/linux/icons");
    fresh_dir(&icons)?;
    let host = builds.iter().find(|(a, _)| a.name == std::env::consts::ARCH).context("no binary for this machine was built")?;
    for size in ICON_SIZES {
        util::render_icon(&host.1.join("plancraft"), &icons.join(format!("{size}.png")), size)?;
    }

    // Packages wait in `dist/linux/untested/` and reach `dist/` only once every one has passed
    // the clean-system test, so a failed run leaves nothing in `dist/` that looks releasable.
    let untested = root.join("dist/linux/untested");
    fresh_dir(&untested)?;
    let mut made = Vec::new();
    for (arch, release) in &builds {
        let (stage, tarball, image) = package_arch(&root, &untested, &version, arch, release, &icons)?;
        for file in [&tarball, &image] {
            let name = file.file_name().context("a package without a file name")?;
            for stale in [root.join("dist").join(name), root.join("dist").join(format!("{}.sha256", name.to_string_lossy()))] {
                if stale.exists() {
                    std::fs::remove_file(&stale).with_context(|| format!("removing the older {}", stale.display()))?;
                }
            }
        }
        made.push((arch, stage, tarball, image));
    }
    for (arch, stage, _, image) in &made {
        smoke_test(&root, arch, stage, image)?;
    }
    let mut results = Vec::new();
    for (_, _, tarball, image) in made {
        for file in [tarball, image] {
            let name = file.file_name().context("a package without a file name")?;
            let done = root.join("dist").join(name);
            std::fs::rename(&file, &done).with_context(|| format!("moving {} into dist/", file.display()))?;
            let sum = util::write_checksum(&done)?;
            artifact(&done);
            artifact(&sum);
            results.extend([done, sum]);
        }
    }
    Ok(results)
}

/// Makes the tarball and the AppImage for one architecture in `out`; returns the staged tarball
/// folder, the tarball and the AppImage.
fn package_arch(root: &Path, out: &Path, version: &str, arch: &Arch, release: &Path, icons: &Path) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let name = format!("plancraft-{version}-linux-{}", arch.name);
    let stage = root.join("dist/linux").join(&name);
    fresh_dir(&stage)?;

    step(format!("staging files ({})", arch.name));
    copy(&release.join("plancraft"), &stage.join("bin/plancraft"), true)?;
    copy(&release.join("plancraft-mcp"), &stage.join("bin/plancraft-mcp"), true)?;
    // PlanCraft Server on its own, for a computer with no desktop (the app runs it as
    // `plancraft serve`).
    copy(&release.join("plancraft-server"), &stage.join("bin/plancraft-server"), true)?;
    bundle_libs(root, arch, &stage.join("lib/plancraft"))?;
    copy(&root.join("README.md"), &stage.join("README.md"), false)?;
    copy(&root.join("LICENSE"), &stage.join("LICENSE"), false)?;
    copy(&root.join("assets/logo.svg"), &stage.join("share/icons/hicolor/scalable/apps/plancraft.svg"), false)?;
    for size in ICON_SIZES {
        let out = stage.join(format!("share/icons/hicolor/{size}x{size}/apps/plancraft.png"));
        copy(&icons.join(format!("{size}.png")), &out, false)?;
    }
    write(&stage.join("share/applications/plancraft.desktop"), DESKTOP_ENTRY, false)?;
    write(&stage.join("share/mime/packages/plancraft.xml"), MIME_XML, false)?;
    write(&stage.join("install.sh"), INSTALL_SH, true)?;

    step(format!("creating the tarball ({})", arch.name));
    let tarball = out.join(format!("{name}.tar.gz"));
    let encoder = GzEncoder::new(File::create(&tarball)?, Compression::best());
    let mut tar = tar::Builder::new(encoder);
    tar.mode(tar::HeaderMode::Deterministic);
    tar.follow_symlinks(false);
    tar.append_dir_all(&name, &stage)?;
    tar.into_inner()?.finish()?;

    let image = appimage(root, out, &stage, version, arch)?;
    Ok((stage, tarball, image))
}

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Builds the release binaries; returns each architecture with the directory its binaries are in.
fn build_binaries(root: &Path) -> Result<Vec<(&'static Arch, PathBuf)>> {
    if std::env::var_os("PLANCRAFT_NATIVE_LINUX").is_some() {
        let arch = ARCHES.iter().find(|a| a.name == std::env::consts::ARCH).context("this machine's architecture is not packaged")?;
        step("building release binaries on this machine (PLANCRAFT_NATIVE_LINUX: they need this machine's glibc or newer)");
        util::build_release(None, &[])?;
        return Ok(vec![(arch, root.join("target/release"))]);
    }
    if std::env::consts::ARCH != "x86_64" {
        bail!("the Linux packages are built on an x86_64 machine (the build container cross-compiles arm64)");
    }
    let installed = output(Command::new("rustup").args(["target", "list", "--installed"]))?;
    let missing: Vec<&str> = ARCHES.iter().map(|a| a.triple).filter(|t| !installed.lines().any(|l| l.trim() == *t)).collect();
    if !missing.is_empty() {
        bail!("missing Rust targets; install them with: rustup target add {}", missing.join(" "));
    }
    step(format!("building release binaries for x86_64 and arm64 in an Ubuntu 22.04 container ({})", BUILD_IMAGE.tag));
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
        .args(["-e", "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc"])
        .arg("-v")
        .arg(format!("{0}:{0}", cargo_home.display()))
        .arg("-v")
        .arg(format!("{0}:{0}:ro", toolchain.display()))
        .arg(BUILD_IMAGE.tag)
        .arg(&cargo)
        .args(["build", "--release", "--locked", "-p", "plancraft-app", "-p", "plancraft-mcp", "-p", "plancraft-server"]);
    for arch in &ARCHES {
        c.args(["--target", arch.triple]);
    }
    run(&mut c)?;
    Ok(ARCHES.iter().map(|a| (a, target.join(a.triple).join("release"))).collect())
}

/// `docker run` as this user, with the repository mounted at its own path; the caller adds the
/// image's tag and the command (the image is made the first time it is needed).
fn container(root: &Path, image: &Image) -> Result<Command> {
    if output(Command::new("docker").args(["image", "inspect", image.tag])).is_err() {
        step(format!("making the container image {} ({})", image.tag, image.about));
        run(Command::new("docker")
            .args(["build", "--platform", image.platform, "-t", image.tag, "-f"])
            .arg(root.join("xtask").join(image.dockerfile))
            .arg(root.join("xtask")))?;
    }
    let uid = output(Command::new("id").arg("-u"))?;
    let gid = output(Command::new("id").arg("-g"))?;
    let mut c = Command::new("docker");
    c.args(["run", "--rm", "--platform", image.platform, "-u"])
        .arg(format!("{}:{}", uid.trim(), gid.trim()))
        .args(["-e", "HOME=/tmp", "-v"])
        .arg(format!("{0}:{0}", root.display()))
        .arg("-w")
        .arg(root);
    Ok(c)
}

/// The AppImage runtime, downloaded once into `target/appimage` and checked against its SHA-256.
fn runtime(root: &Path, arch: &Arch) -> Result<Vec<u8>> {
    let path = root.join(format!("target/appimage/{}-20251108", arch.runtime));
    if !path.exists() {
        step(format!("downloading the AppImage runtime ({})", arch.name));
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let part = path.with_extension("part");
        run(Command::new("curl").args(["-fsSL", "--proto", "=https", "-o"]).arg(&part).arg(format!("{RUNTIME_RELEASE}/{}", arch.runtime)))?;
        std::fs::rename(&part, &path)?;
    }
    let bytes = std::fs::read(&path)?;
    if hex(&bytes) != arch.runtime_sha256 {
        let _ = std::fs::remove_file(&path);
        bail!("the AppImage runtime's checksum is wrong; it was removed (run the command again to download it afresh)");
    }
    Ok(bytes)
}

/// Builds `PlanCraft-<version>-<arch>.AppImage` in `out` from the staged tarball files.
fn appimage(root: &Path, out: &Path, stage: &Path, version: &str, arch: &Arch) -> Result<PathBuf> {
    let runtime = runtime(root, arch)?;
    step(format!("staging the AppImage ({})", arch.name));
    let appdir = root.join(format!("dist/linux/PlanCraft-{}.AppDir", arch.name));
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

    step(format!("packing the AppImage ({})", arch.name));
    let squashfs = root.join(format!("dist/linux/PlanCraft-{}.squashfs", arch.name));
    let _ = std::fs::remove_file(&squashfs);
    let mut c = container(root, &BUILD_IMAGE)?;
    c.args([BUILD_IMAGE.tag, "mksquashfs"]).arg(&appdir).arg(&squashfs).args(["-root-owned", "-noappend", "-comp", "zstd", "-quiet"]);
    run(&mut c)?;
    let image = out.join(format!("PlanCraft-{version}-{}.AppImage", arch.name));
    let mut bytes = runtime;
    bytes.extend(std::fs::read(&squashfs)?);
    std::fs::write(&image, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&image, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(image)
}

/// The folder a bundled library goes in: its soname without the version (`libXi.so.6` → `libXi`).
fn lib_dir(soname: &str) -> &str {
    soname.split(".so").next().unwrap_or(soname)
}

/// Copies `BUNDLED_LIBS` for `arch` out of the build image, each into `dest/<name>/` under its
/// soname (symlinks resolved).
fn bundle_libs(root: &Path, arch: &Arch, dest: &Path) -> Result<()> {
    step(format!("bundling the window-system libraries ({})", arch.name));
    fresh_dir(dest)?;
    for lib in BUNDLED_LIBS {
        std::fs::create_dir_all(dest.join(lib_dir(lib)))?;
    }
    let script = r#"from=$1; dest=$2; shift 2; for l in "$@"; do cp -L "$from/$l" "$dest/${l%%.so*}/" || exit 1; done"#;
    let mut c = container(root, &BUILD_IMAGE)?;
    c.args([BUILD_IMAGE.tag, "sh", "-c", script, "sh", arch.lib_dir]).arg(dest).args(BUNDLED_LIBS);
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

/// Starts both packages for `arch` on a clean system (see `SMOKE_TEST`); fails the build if
/// either does not open its window. `PLANCRAFT_SKIP_SMOKE_TEST=1` skips it (never for a release).
fn smoke_test(root: &Path, arch: &Arch, stage: &Path, image: &Path) -> Result<()> {
    if smoke_test_skipped() {
        step(format!("SKIPPING the clean-system test for {} (PLANCRAFT_SKIP_SMOKE_TEST): do not release these packages", arch.name));
        return Ok(());
    }
    if std::env::var_os("PLANCRAFT_NATIVE_LINUX").is_some() {
        step("skipping the clean-system test: PLANCRAFT_NATIVE_LINUX binaries need this machine's newer glibc");
        return Ok(());
    }
    // Emulated, starting takes a minute or two instead of seconds.
    let (limit, emulator) = match arch.emulator {
        None => (180, None),
        Some(entry) => (900, Some(emulator(entry, arch)?)),
    };
    let how = if arch.emulator.is_some() { ", emulated" } else { "" };
    step(format!("starting the {} packages on a clean Ubuntu 22.04 desktop{how}", arch.name));
    let out = root.join("dist/linux/smoke").join(arch.name);
    fresh_dir(&out)?;
    let mut c = container(root, &arch.smoke_image)?;
    let inside = "/opt/plancraft-emulator";
    if let Some(path) = &emulator {
        c.arg("-v").arg(format!("{}:{inside}:ro", path.display()));
    }
    c.args([arch.smoke_image.tag, "sh", "-c", SMOKE_TEST, "sh"]).arg(&out).arg(image).arg(stage).arg(limit.to_string());
    c.arg(if emulator.is_some() { inside } else { "" });
    run(&mut c).with_context(|| {
        format!(
            "a {} Linux package does not start on a clean system (the log is above; screenshots and logs in {})",
            arch.name,
            out.display()
        )
    })
}

/// How to get an emulator registered the way the arm64 test needs it: with the `F` flag and a
/// statically linked QEMU (it is also mounted into the test container). Older releases register
/// their dynamically linked `qemu-user` without `F`; their static build is `qemu-user-static`.
const QEMU_ADVICE: &str = "install it: sudo apt install qemu-user-static, or on newer releases where that package no longer \
     exists (Ubuntu 26.04, for one), sudo apt install qemu-user-binfmt";

/// `PLANCRAFT_SKIP_SMOKE_TEST=1`: package without the clean-system test (never for a release).
fn smoke_test_skipped() -> bool {
    std::env::var_os("PLANCRAFT_SKIP_SMOKE_TEST").is_some()
}

/// Checks that this machine runs `arch`'s programs through QEMU, in containers too (the `F`
/// flag: the kernel opens the emulator when the entry is registered, so it works inside a
/// container that lacks it); returns the emulator's path.
fn emulator(entry: &str, arch: &Arch) -> Result<PathBuf> {
    let path = Path::new("/proc/sys/fs/binfmt_misc").join(entry);
    let status = std::fs::read_to_string(&path).unwrap_or_default();
    let enabled = status.lines().next() == Some("enabled");
    let fixed = status.lines().any(|l| l.strip_prefix("flags:").is_some_and(|f| f.contains('F')));
    let interpreter = status.lines().find_map(|l| l.strip_prefix("interpreter ")).map(|p| PathBuf::from(p.trim()));
    match interpreter {
        Some(interpreter) if enabled && fixed && interpreter.is_file() => Ok(interpreter),
        _ => bail!(
            "the {} packages are tested under QEMU emulation, which this machine lacks ({} must be enabled, with the F flag, \
             and name an emulator that exists on this machine); \
             {QEMU_ADVICE}, or set PLANCRAFT_SKIP_SMOKE_TEST=1 for a package that is not a release",
            arch.name,
            path.display()
        ),
    }
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
