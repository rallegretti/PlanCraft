//! Embeds the application icon into the Windows executable.
//!
//! Writes a Win32 `.res` resource file directly from `assets/plancraft.ico`
//! (one RT_ICON per image plus an RT_GROUP_ICON directory) and hands it to the
//! MSVC linker, so no resource compiler is needed. Does nothing on other targets.

use std::path::PathBuf;

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const LANG_EN_US: u16 = 0x0409;

/// One resource entry with ordinal type and name (32-byte header), data padded to 4 bytes.
fn push_resource(out: &mut Vec<u8>, kind: u16, id: u16, memory_flags: u16, language: u16, data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_le_bytes()); // DataSize
    out.extend_from_slice(&32u32.to_le_bytes()); // HeaderSize
    out.extend_from_slice(&[0xFF, 0xFF]);
    out.extend_from_slice(&kind.to_le_bytes()); // TYPE (ordinal)
    out.extend_from_slice(&[0xFF, 0xFF]);
    out.extend_from_slice(&id.to_le_bytes()); // NAME (ordinal)
    out.extend_from_slice(&0u32.to_le_bytes()); // DataVersion
    out.extend_from_slice(&memory_flags.to_le_bytes());
    out.extend_from_slice(&language.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // Version
    out.extend_from_slice(&0u32.to_le_bytes()); // Characteristics
    out.extend_from_slice(data);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

fn icon_res(ico: &[u8]) -> Vec<u8> {
    let u16_at = |i: usize| u16::from_le_bytes([ico[i], ico[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([ico[i], ico[i + 1], ico[i + 2], ico[i + 3]]);
    assert!(ico.len() >= 6 && u16_at(2) == 1, "assets/plancraft.ico is not an icon file");
    let count = u16_at(4) as usize;

    let mut out = Vec::new();
    // A .res file starts with an empty sentinel entry.
    push_resource(&mut out, 0, 0, 0, 0, &[]);

    let mut group = Vec::new();
    group.extend_from_slice(&0u16.to_le_bytes());
    group.extend_from_slice(&1u16.to_le_bytes());
    group.extend_from_slice(&(count as u16).to_le_bytes());
    for i in 0..count {
        let e = 6 + 16 * i;
        let size = u32_at(e + 8) as usize;
        let offset = u32_at(e + 12) as usize;
        let id = (i + 1) as u16;
        push_resource(&mut out, RT_ICON, id, 0x1010, LANG_EN_US, &ico[offset..offset + size]);
        // GRPICONDIRENTRY: the ICONDIRENTRY fields with a resource id instead of a file offset.
        group.extend_from_slice(&ico[e..e + 12]);
        group.extend_from_slice(&id.to_le_bytes());
    }
    push_resource(&mut out, RT_GROUP_ICON, 1, 0x1030, LANG_EN_US, &group);
    out
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let ico_path = manifest.join("../../assets/plancraft.ico");
    println!("cargo:rerun-if-changed={}", ico_path.display());
    println!("cargo:rerun-if-changed=build.rs");

    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if !(windows && msvc) {
        return;
    }
    let ico = std::fs::read(&ico_path).expect("reading assets/plancraft.ico");
    let res = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("plancraft-icon.res");
    std::fs::write(&res, icon_res(&ico)).expect("writing icon resource");
    // The MSVC linker accepts compiled resource files as ordinary inputs.
    println!("cargo:rustc-link-arg-bin=plancraft={}", res.display());
}
