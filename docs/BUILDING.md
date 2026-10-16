# Building and packaging PlanCraft

Release packages are made with `cargo xtask`. This is a small Rust program in `xtask/` that builds the app and packages it for each platform. When you ask for a platform other than the one you're on, it does the work on a build machine over SSH. No cloud CI is involved.

| Platform | Package | Contents |
|---|---|---|
| macOS | `PlanCraft-<version>-macos-universal.dmg` | `PlanCraft.app` (Apple Silicon + Intel), with the headless `plancraft-mcp` inside the bundle |
| Windows | `PlanCraft-<version>-windows-x64.zip` | portable `PlanCraft.exe` (GUI, with icon) and the console `plancraft-mcp.exe` |
| Linux | `plancraft-<version>-linux-x86_64.tar.gz` | binaries, `install.sh`, desktop entry, icons, `.plancraft` MIME type |

Every package gets a `.sha256` checksum file next to it. Packages are **not code-signed** yet:

- **macOS:** the app is ad-hoc signed. On first launch, right-click it and choose *Open*.
- **Windows:** SmartScreen shows "Windows protected your PC". Choose *More info* ▸ *Run anyway*.

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

- **Xcode** (or the Command Line Tools), for `lipo`, `codesign` and `hdiutil`.
- **Rust targets:** `rustup target add aarch64-apple-darwin x86_64-apple-darwin`.
- **SSH:** turn on *System Settings ▸ General ▸ Sharing ▸ Remote Login*.

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

- **macOS:** `lipo`, `codesign` and `hdiutil`, which ship with macOS and Xcode.
- **Windows:** the MSVC linker, which every Windows Rust build needs anyway.
- **Linux:** `install.sh` calls the desktop's own update tools to refresh menus and icons.
