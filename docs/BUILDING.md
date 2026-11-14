# Building and packaging PlanCraft

Release packages are made with `cargo xtask`. This is a small Rust program in `xtask/` that builds the app and packages it for each platform. When you ask for a platform other than the one you're on, it does the work on a build machine over SSH. Everything is built and tested on your own machines. There is no cloud CI and no GitHub Actions.

| Platform | Package | Contents |
|---|---|---|
| macOS | `PlanCraft-<version>-macos-universal.dmg` | `PlanCraft.app` (Apple Silicon + Intel), with the headless `plancraft-mcp` inside the bundle |
| Windows | `PlanCraft-<version>-windows-x64.zip` | portable `PlanCraft.exe` (GUI, with icon) and the console `plancraft-mcp.exe` |
| Linux | `plancraft-<version>-linux-x86_64.tar.gz` | binaries, `install.sh`, desktop entry, icons, `.plancraft` MIME type |
| Linux | `PlanCraft-<version>-x86_64.AppImage` | the same app as one file that runs without installing |

The Linux binaries are built in an Ubuntu 22.04 container (`xtask/linux-build.Dockerfile`, image `plancraft-build:ubuntu22.04`, made the first time). They need only glibc 2.35, so both Linux packages run on Ubuntu 22.04 and every newer release; built directly on a newer system they would need its glibc. The build uses this machine's Rust toolchain, mounted read-only, and Docker must be installed. `PLANCRAFT_NATIVE_LINUX=1 cargo xtask package linux` builds without the container for a quick local package that runs only on systems at least as new as this one.

Both Linux packages must work for someone who has none of our software installed. They carry the window-system libraries the app loads at startup that a fresh desktop may lack, each in its own folder under `lib/plancraft/`. They only fill gaps. At startup the app checks which of them the system lacks, looking in the loader's folders from `/etc/ld.so.conf` and the standard ones. If any are missing, it restarts itself once with just those folders on `LD_LIBRARY_PATH` (`crates/plancraft-app/src/bundled_libs.rs`). A library the system has is always used from the system, so it matches the system's keyboard data, display server and drivers. The bundled libraries are:
- libxkbcommon and libxkbcommon-x11 with libxcb-xkb (keyboard maps);
- libXcursor, libXrandr, libXi and libXrender (cursors, screens, input devices);
- libwayland-egl.

They are copied from the build image (`BUNDLED_LIBS` in `xtask/src/linux.rs`). glibc, libX11/libxcb, libwayland-client and the graphics drivers (libGL, libEGL, Vulkan, Mesa) are never bundled: every desktop has them, and they must match the system's own drivers and display server.

Before a Linux build finishes, `cargo xtask package linux` tests both packages on a clean system, a container made from `xtask/linux-smoke.Dockerfile`. That's Ubuntu 22.04 with only a display server (Xvfb) and Mesa, as on a fresh install. Each bundled library must find everything it needs there, and the AppImage and the tarball's binary must each open their window and save a screenshot. The build fails otherwise, and the logs and screenshots are left in `dist/linux/smoke/`. `PLANCRAFT_SKIP_SMOKE_TEST=1` skips the test for a quick local package; never release one built that way.

The AppImage starts with the AppImage project's type-2 runtime. `cargo xtask` downloads it once from the project's GitHub release `20251108` into `target/appimage/`, and refuses it unless its SHA-256 matches the one pinned in `xtask/src/linux.rs`. The app files are packed with `mksquashfs` (zstd) in the same container.

Every package gets a `.sha256` checksum file next to it.

- **macOS:** the app and the disk image are signed with the Developer ID certificate and notarized by Apple, with the tickets stapled, so they open without a Gatekeeper warning (see [macOS signing](#macos-signing)).
- **Windows:** not code-signed yet. SmartScreen shows "Windows protected your PC". Choose *More info* ▸ *Run anyway*.
- **Linux:** not signed; check the `.sha256`.

## Commands

```bash
cargo xtask package <mac|linux|windows>
```

Builds and packages on this machine. The target must match the operating system you're on.

```bash
cargo xtask dist <mac|linux|windows|all> [--host SSH_ALIAS]
```

Packages locally when the target matches this machine; otherwise it uses the target's build host. `all` covers every platform that is either local or has a host configured. Finished packages are collected in `dist/`.

```bash
cargo xtask icons
```

Regenerates `assets/plancraft.ico`, the icon embedded in `PlanCraft.exe`, from the logo. Run it after changing the logo, then commit the new file.

## Checks

```bash
cargo xtask ci
```

Runs every quality gate locally:

- `cargo fmt --check`;
- `cargo clippy --workspace --all-targets -- -D warnings`, including the never-crash lints (`unwrap`, `expect`, `panic!` and indexing are denied in shipped code);
- `cargo test --workspace`: unit tests, the hostile-input sweeps (`crates/plancraft-core/tests/hostile.rs`, `crates/plancraft-mcp/tests/garbage.rs`) and the Microsoft Project parity tests (`crates/plancraft-core/tests/ms_parity.rs`);
- the scheduler speed gate in release mode (200 tasks must recalculate in under 10 ms);
- `cargo deny check`: licences, security advisories, banned crates (nothing that compiles C) and sources. Install it once with `cargo install --locked cargo-deny`.

Run it on each build host too (over SSH, from the copy `cargo xtask dist` sends to `~/Builds/PlanCraft`) to catch platform-specific problems before a release.

The property tests use random inputs. To search harder locally, raise the number of cases, for example `PROPTEST_CASES=5000 cargo test -p plancraft-core --test hostile`. When one fails, proptest saves the failing input in a `*.proptest-regressions` file next to the test; commit it so the case is always re-run.

## Build hosts

A build host is any machine you can reach by SSH without a password prompt, with Rust installed via rustup. Name your hosts in `dist-hosts.toml` at the repository root. The file is git-ignored; copy `dist-hosts.example.toml` to start:

```toml
mac = "my-mac"            # SSH aliases from ~/.ssh/config
windows = "my-windows-pc"
```

You can also pass `--host`, or set `PLANCRAFT_MAC_HOST` / `PLANCRAFT_WINDOWS_HOST`.

For each remote build, `cargo xtask dist` does the following:

1. Sends the current source to `~/Builds/PlanCraft` on the host, using `git archive` piped through `ssh`.
   - This covers `HEAD` plus uncommitted changes to tracked files.
   - Untracked files are *not* sent; `git add` new files first.
   - The host's `target/` directory is kept, so rebuilds are incremental.
2. Runs `cargo xtask package <target>` on the host.
3. Copies every package the host reports back into `dist/` with `scp`.

### macOS host

- **Xcode** (or the Command Line Tools), for `lipo`, `codesign`, `hdiutil`, `notarytool` and `stapler`.
- **Rust targets:** `rustup target add aarch64-apple-darwin x86_64-apple-darwin`.
- **SSH:** turn on *System Settings ▸ General ▸ Sharing ▸ Remote Login*.
- **Signing:** see below.

### macOS signing

`cargo xtask package mac` (and `dist mac`) signs `PlanCraft.app` with the *Developer ID Application* certificate in the Mac's keychain, using the hardened runtime and a secure timestamp. It sends the app to Apple's notary service and staples the ticket to it, then signs, notarizes and staples the disk image, and finally checks both with Gatekeeper (`spctl`). Notarizing takes a few minutes per upload. It needs:

- **The certificate** in the login keychain. With exactly one Developer ID certificate it is found automatically; with several, set `PLANCRAFT_SIGN_IDENTITY` to its name or SHA-1.
- **A notarytool profile** in the keychain, whose name is in `~/.config/signing/notary-profile` on the Mac (`PLANCRAFT_NOTARY_PROFILE` overrides it). One profile notarizes every app of your team; you only need one app-specific password per Mac, not per app. To make a profile (it asks for your Apple ID, team ID and an app-specific password):
  ```bash
  xcrun notarytool store-credentials <name>
  ```
- **An unlocked login keychain.** It is locked in every SSH session, even when you're signed in at the Mac. On a Mac used only for signing, save the login password once in an owner-only file, and `cargo xtask` unlocks the keychain with it:
  ```bash
  ssh -t <mac-host> 'umask 077 && mkdir -p ~/.config/signing && read -rs "p?Mac password: " && printf %s "$p" > ~/.config/signing/keychain-password'
  ```
  (The Mac's shell is zsh, hence `read "p?prompt"`.) Without the file, run `security unlock-keychain` and the build in the same SSH session.

Everything is checked before the build starts. The `PLANCRAFT_*` settings above are passed on to the build host when `dist` runs remotely. Credentials never leave the Mac, and the password is never printed. Both files must be readable by their owner only, or the build stops. `PLANCRAFT_ADHOC_SIGN=1` makes an ad-hoc signed test build instead (not for release: Gatekeeper warns about it).

### Windows host

1. **Install and start OpenSSH Server**, allow port 22 from the LAN, make PowerShell the default SSH shell, and add your public key. For an administrator account, the key goes in `C:\ProgramData\ssh\administrators_authorized_keys`.
2. **Install the Visual Studio Build Tools** with the C++ workload, for the MSVC linker and the Windows SDK:
   ```powershell
   winget install --id Microsoft.VisualStudio.BuildTools -e --override "--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
   ```
3. **Install Rust:**
   ```powershell
   winget install --id Rustlang.Rustup -e
   ```
   Then make the Windows (MSVC) toolchain the default:
   ```powershell
   rustup default stable-x86_64-pc-windows-msvc
   ```

Commands sent to Windows are base64-encoded PowerShell (`-EncodedCommand`), so quoting survives the trip through SSH and the Windows command line.

## Pure Rust

The packaging logic lives in Rust; even the `.icns`, `.ico` and `.res` icon formats are written by our own code. A few platform tools are still used:

- **macOS:** `lipo`, `codesign`, `hdiutil`, `notarytool`, `stapler` and `spctl`, which ship with macOS and Xcode.
- **Windows:** the MSVC linker, which every Windows Rust build needs anyway.
- **Linux:** `install.sh` calls the desktop's own update tools to refresh menus and icons.
