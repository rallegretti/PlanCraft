//! Windows packaging: a portable `.zip` with `PlanCraft.exe` (GUI, icon
//! embedded by the app's build script) and the console `plancraft-mcp.exe`.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use zip::write::SimpleFileOptions;

use crate::util::{self, artifact, copy, fresh_dir, step};

const README: &str = "PlanCraft for Windows (portable)
================================

Run PlanCraft.exe. No installation is needed; you can move this folder anywhere
(for example to C:\\Program Files\\PlanCraft or your Documents folder).

This build is not code-signed yet, so Windows SmartScreen may say
\"Windows protected your PC\". Choose \"More info\", then \"Run anyway\".

AI agents (MCP)
---------------
Live, in the running app: Project > AI Agents > Start server, then
    claude mcp add --transport http plancraft http://127.0.0.1:7878/mcp

Headless, working on files:
    claude mcp add plancraft -- \"C:\\path\\to\\plancraft-mcp.exe\" \"C:\\path\\to\\project.plancraft\"

See README.md for everything else.
";

pub fn package() -> Result<Vec<PathBuf>> {
    if !cfg!(target_os = "windows") {
        bail!("Windows packages must be built on Windows (use `cargo xtask dist windows` to build on a remote PC)");
    }
    let root = util::root();
    let version = util::version()?;
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };

    step("building release binaries");
    util::build_release(None, &[])?;

    let name = format!("PlanCraft-{version}-windows-{arch}");
    let stage = root.join("dist/windows").join(&name);
    fresh_dir(&stage)?;

    step("staging files");
    let release = root.join("target/release");
    copy(&release.join("plancraft.exe"), &stage.join("PlanCraft.exe"), true)?;
    copy(&release.join("plancraft-mcp.exe"), &stage.join("plancraft-mcp.exe"), true)?;
    copy(&root.join("README.md"), &stage.join("README.md"), false)?;
    copy(&root.join("LICENSE"), &stage.join("LICENSE.txt"), false)?;
    std::fs::write(stage.join("README-Windows.txt"), README.replace('\n', "\r\n"))?;

    step("creating the zip archive");
    let zip_path = root.join("dist").join(format!("{name}.zip"));
    write_zip(&stage, &name, &zip_path)?;

    let sum = util::write_checksum(&zip_path)?;
    artifact(&zip_path);
    artifact(&sum);
    Ok(vec![zip_path, sum])
}

/// Zips the files of `dir` (flat) under a top-level folder named `folder`.
fn write_zip(dir: &Path, folder: &str, out: &Path) -> Result<()> {
    let mut zip = zip::ZipWriter::new(File::create(out)?);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).compression_level(Some(9));
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for path in entries {
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name() else { continue };
        let name = format!("{folder}/{}", file_name.to_string_lossy());
        zip.start_file(name, options)?;
        zip.write_all(&std::fs::read(&path)?)?;
    }
    zip.finish()?;
    Ok(())
}
