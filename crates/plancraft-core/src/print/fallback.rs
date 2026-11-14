//! Fallback fonts from the operating system, for characters the bundled font lacks (Chinese,
//! Japanese, Korean, Hebrew, Arabic, Thai, Devanagari and more).
//!
//! Finding them is cheap: only each candidate's character map is read. A font's whole file is
//! read the first time a character needs it ([`Fallback::face_data`]). Well-known fonts that ship
//! with each system are tried first, in order of preference (the language in `LANG` decides which
//! Chinese, Japanese or Korean font comes first); then any other installed font that covers a
//! script still missing.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use skrifa::raw::tables::cmap::Cmap;
use skrifa::raw::{FontData, FontRead};

/// Largest font file read (some CJK collections are tens of megabytes).
const MAX_FILE: u64 = 160 * 1024 * 1024;
/// Largest character map read.
const MAX_CMAP: u32 = 8 * 1024 * 1024;
/// Most fallback fonts used.
const MAX_FALLBACKS: usize = 12;
/// Font files whose character maps are looked at beyond the well-known ones.
const MAX_SWEEP: usize = 600;
/// Directory entries looked at while searching the font folders.
const MAX_ENTRIES: usize = 50_000;

/// One character from each script worth a fallback font. Latin, Greek and Cyrillic are in the
/// bundled font already.
const PROBES: &[char] =
    &['漢', 'あ', 'ア', '한', 'א', 'ع', 'ก', 'अ', 'ক', 'த', 'ა', 'Ա', 'አ', 'ພ', 'ក', 'မ', 'ᐃ', 'ꆈ', 'ܐ', 'ހ', '中', '日', '語'];

/// A fallback font file and face, with its character map.
pub struct Fallback {
    pub path: PathBuf,
    pub index: u32,
    /// Name for PDFs (letters, digits and hyphens).
    pub name: String,
    /// CFF outlines (embedded differently in PDFs from TrueType outlines).
    pub cff: bool,
    cmap: Vec<u8>,
    data: OnceLock<Option<&'static [u8]>>,
}

impl Fallback {
    /// Whether the font has a glyph for `ch`.
    pub fn covers(&self, ch: char) -> bool {
        self.glyph(ch).is_some_and(|g| g != 0)
    }

    /// The glyph id for `ch` from the character map read at discovery.
    pub fn glyph(&self, ch: char) -> Option<u16> {
        let cmap = Cmap::read(FontData::new(&self.cmap)).ok()?;
        cmap.map_codepoint(ch).and_then(|g| u16::try_from(g.to_u32()).ok())
    }

    /// The whole font file, read once and kept for the life of the program (the app's text
    /// engine and the PDF writer share it). `None` if it can no longer be read.
    pub fn face_data(&self) -> Option<&'static [u8]> {
        *self.data.get_or_init(|| {
            let len = std::fs::metadata(&self.path).ok()?.len();
            if len > MAX_FILE {
                return None;
            }
            let bytes = std::fs::read(&self.path).ok()?;
            // Still a readable font with the same face (the file could have been replaced).
            skrifa::FontRef::from_index(&bytes, self.index).ok()?;
            Some(&*Box::leak(bytes.into_boxed_slice()))
        })
    }
}

/// The fallback fonts on this system, found once. Set `PLANCRAFT_SYSTEM_FONTS=0` to use only the
/// bundled font (screenshots and tests that must look the same on every machine).
pub fn fallbacks() -> &'static [Fallback] {
    static ALL: OnceLock<Vec<Fallback>> = OnceLock::new();
    ALL.get_or_init(|| {
        if std::env::var_os("PLANCRAFT_SYSTEM_FONTS").is_some_and(|v| v == "0") {
            return Vec::new();
        }
        crate::guard::catch("finding system fonts", || discover(&font_dirs(), &language())).unwrap_or_default()
    })
}

/// Starts finding the fallback fonts in the background, so the first character that needs one
/// does not wait.
pub fn warm_up() {
    let _ = std::thread::Builder::new().name("fonts".into()).spawn(|| {
        let _ = fallbacks();
    });
}

/// The user's language from the environment, such as "ja" or "zh_TW" (empty when unknown).
fn language() -> String {
    ["LC_ALL", "LC_CTYPE", "LANG", "LANGUAGE"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|v| v.to_string_lossy().to_string())
        .find(|v| !v.is_empty() && v != "C" && v != "POSIX")
        .unwrap_or_default()
}

/// Folders that hold fonts on this system.
fn font_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dirs = Vec::new();
    if cfg!(windows) {
        let windir = std::env::var_os("WINDIR").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        dirs.push(windir.join("Fonts"));
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join(r"Microsoft\Windows\Fonts"));
        }
    } else if cfg!(target_os = "macos") {
        dirs.push("/System/Library/Fonts".into());
        dirs.push("/Library/Fonts".into());
        if let Some(h) = &home {
            dirs.push(h.join("Library/Fonts"));
        }
    } else {
        if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
            dirs.push(PathBuf::from(data).join("fonts"));
        }
        if let Some(h) = &home {
            dirs.push(h.join(".local/share/fonts"));
            dirs.push(h.join(".fonts"));
        }
        let data_dirs = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
        for d in data_dirs.split(':').filter(|d| !d.is_empty()) {
            dirs.push(PathBuf::from(d).join("fonts"));
        }
        dirs.push("/usr/share/fonts".into());
        dirs.push("/usr/local/share/fonts".into());
    }
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
    dirs
}

/// Well-known fallback fonts in order of preference: file name and face index in the file.
fn preferred(lang: &str) -> Vec<(&'static str, u32)> {
    let lang = lang.to_ascii_lowercase();
    // Chinese, Japanese and Korean share many characters but draw them differently: the user's
    // language picks which comes first. Noto/Source Han collections hold JP, KR, SC, TC, HK.
    let (cjk_index, order): (u32, [&str; 4]) = if lang.starts_with("ja") {
        (0, ["ja", "zh", "tw", "ko"])
    } else if lang.starts_with("ko") {
        (1, ["ko", "zh", "ja", "tw"])
    } else if lang.starts_with("zh_tw") || lang.starts_with("zh-tw") || lang.starts_with("zh_hk") || lang.starts_with("zh-hk") {
        (3, ["tw", "zh", "ja", "ko"])
    } else {
        (2, ["zh", "ja", "tw", "ko"])
    };
    let cjk = |which: &str| -> Vec<(&'static str, u32)> {
        match which {
            "zh" => vec![
                ("msyh.ttc", 0),
                ("simsun.ttc", 0),
                ("PingFang.ttc", 0),
                ("Hiragino Sans GB.ttc", 0),
                ("STHeiti Light.ttc", 0),
                ("NotoSansCJKsc-Regular.otf", 0),
                ("NotoSansSC-Regular.otf", 0),
                ("wqy-microhei.ttc", 0),
                ("wqy-zenhei.ttc", 0),
            ],
            "ja" => vec![
                ("YuGothR.ttc", 0),
                ("YuGothM.ttc", 0),
                ("meiryo.ttc", 0),
                ("msgothic.ttc", 0),
                ("ヒラギノ角ゴシック W3.ttc", 0),
                ("NotoSansCJKjp-Regular.otf", 0),
                ("NotoSansJP-Regular.otf", 0),
                ("ipagp.ttf", 0),
                ("TakaoPGothic.ttf", 0),
            ],
            "tw" => vec![("msjh.ttc", 0), ("mingliub.ttc", 0), ("NotoSansCJKtc-Regular.otf", 0), ("NotoSansTC-Regular.otf", 0)],
            _ => vec![
                ("malgun.ttf", 0),
                ("AppleSDGothicNeo.ttc", 0),
                ("NotoSansCJKkr-Regular.otf", 0),
                ("NotoSansKR-Regular.otf", 0),
                ("NanumGothic.ttf", 0),
            ],
        }
    };
    let mut out: Vec<(&'static str, u32)> = vec![
        // One collection for all four, where installed (most Linux desktops).
        ("NotoSansCJK-Regular.ttc", cjk_index),
        ("NotoSansCJK-Regular.otc", cjk_index),
        ("SourceHanSans-Regular.ttc", cjk_index),
    ];
    for which in order {
        out.extend(cjk(which));
    }
    out.extend([
        // Hebrew, Arabic and many more in one font.
        ("segoeui.ttf", 0),
        ("arial.ttf", 0),
        ("Arial.ttf", 0),
        ("ArialHB.ttc", 0),
        ("LucidaGrande.ttc", 0),
        ("NotoSans-Regular.ttf", 0),
        ("DejaVuSans.ttf", 0),
        ("NotoSansHebrew-Regular.ttf", 0),
        ("NotoSansArabic-Regular.ttf", 0),
        // Thai, Lao, Khmer.
        ("leelawui.ttf", 0),
        ("tahoma.ttf", 0),
        ("Thonburi.ttc", 0),
        ("NotoSansThai-Regular.ttf", 0),
        // Indian scripts.
        ("Nirmala.ttc", 0),
        ("Nirmala.ttf", 0),
        ("Kohinoor.ttc", 0),
        ("NotoSansDevanagari-Regular.ttf", 0),
        // Armenian, Georgian, Ethiopic and others.
        ("sylfaen.ttf", 0),
        ("ebrima.ttf", 0),
        ("gadugi.ttf", 0),
        ("mmrtext.ttf", 0),
        ("NotoSansGeorgian-Regular.ttf", 0),
        ("NotoSansArmenian-Regular.ttf", 0),
        ("NotoSansEthiopic-Regular.ttf", 0),
        // Very wide coverage, last because of their size.
        ("Arial Unicode.ttf", 0),
        ("arialuni.ttf", 0),
        ("DroidSansFallbackFull.ttf", 0),
        ("DroidSansFallback.ttf", 0),
        ("FreeSans.ttf", 0),
    ]);
    out
}

/// Font files under `dirs` by lower-case file name (the first found wins), searched a few
/// levels deep.
fn font_files(dirs: &[PathBuf]) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut entries = 0usize;
    let mut todo: Vec<(PathBuf, u8)> = dirs.iter().rev().map(|d| (d.clone(), 0)).collect();
    while let Some((dir, depth)) = todo.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else { continue };
        let mut subdirs = Vec::new();
        let mut list: Vec<std::fs::DirEntry> = read.flatten().collect();
        list.sort_by_key(std::fs::DirEntry::file_name);
        for e in list {
            entries += 1;
            if entries > MAX_ENTRIES {
                return out;
            }
            let path = e.path();
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_dir() || (kind.is_symlink() && path.is_dir()) {
                if depth < 5 {
                    subdirs.push((path, depth + 1));
                }
                continue;
            }
            let name = e.file_name().to_string_lossy().to_string();
            let lower = name.to_lowercase();
            if [".ttf", ".otf", ".ttc", ".otc"].iter().any(|x| lower.ends_with(x)) && seen.insert(lower.clone()) {
                out.push((lower, path));
            }
        }
        todo.extend(subdirs.into_iter().rev());
    }
    out
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(at)?, *b.get(at + 1)?]))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes([*b.get(at)?, *b.get(at + 1)?, *b.get(at + 2)?, *b.get(at + 3)?]))
}

fn read_at(f: &mut File, at: u64, len: usize) -> Option<Vec<u8>> {
    f.seek(SeekFrom::Start(at)).ok()?;
    let mut buf = vec![0; len];
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

/// Reads face `index`'s character map and outline kind without reading the whole file.
fn probe_file(path: &Path, index: u32) -> Option<(Vec<u8>, bool)> {
    let mut f = File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    if len > MAX_FILE {
        return None;
    }
    let head = read_at(&mut f, 0, 12)?;
    let offset = if head.get(..4) == Some(b"ttcf".as_slice()) {
        let count = u32_at(&head, 8)?;
        if index >= count {
            return None;
        }
        let at = read_at(&mut f, 12 + u64::from(index) * 4, 4)?;
        u64::from(u32_at(&at, 0)?)
    } else if index == 0 {
        0
    } else {
        return None;
    };
    let dir = read_at(&mut f, offset, 12)?;
    let tables = usize::from(u16_at(&dir, 4)?.min(512));
    let recs = read_at(&mut f, offset + 12, tables * 16)?;
    let mut cmap = None;
    let mut cff = false;
    let mut glyf = false;
    for i in 0..tables {
        let r = recs.get(i * 16..i * 16 + 16)?;
        let tag = r.get(..4)?;
        match tag {
            b"cmap" => cmap = Some((u32_at(r, 8)?, u32_at(r, 12)?)),
            b"CFF " => cff = true,
            b"glyf" => glyf = true,
            _ => {}
        }
    }
    // CFF2 (variable CFF) fonts are not subset for PDFs; bitmap-only fonts have no outlines.
    if !(cff || glyf) {
        return None;
    }
    let (at, size) = cmap?;
    if size > MAX_CMAP || u64::from(at) + u64::from(size) > len {
        return None;
    }
    let data = read_at(&mut f, u64::from(at), usize::try_from(size).ok()?)?;
    Cmap::read(FontData::new(&data)).ok()?;
    Some((data, cff && !glyf))
}

/// A PDF-safe name for a font file and face.
fn pdf_name(path: &Path, index: u32) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let mut name: String = stem.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(40).collect();
    if name.is_empty() {
        name = "Fallback".into();
    }
    if index > 0 {
        name.push_str(&format!("-{index}"));
    }
    name
}

/// Words in file names of faces that make poor fallbacks (styles, symbols, monospace).
const SKIP_WORDS: &[&str] = &[
    "bold",
    "italic",
    "oblique",
    "black",
    "heavy",
    "thin",
    "light",
    "condensed",
    "narrow",
    "mono",
    "code",
    "emoji",
    "symbol",
    "math",
    "icon",
    "awesome",
    "serif",
    "semibold",
    "medium",
    "extrabold",
    "ultra",
];

/// Picks fallback fonts from the font files in `dirs` so that as many [`PROBES`] as possible
/// are covered, starting from the characters the bundled font lacks.
fn discover(dirs: &[PathBuf], lang: &str) -> Vec<Fallback> {
    let files = font_files(dirs);
    let by_name: HashMap<&str, &PathBuf> = files.iter().map(|(n, p)| (n.as_str(), p)).collect();
    let mut missing: Vec<char> = PROBES.iter().copied().filter(|c| !super::font::bundled_has(*c)).collect();
    let mut out: Vec<Fallback> = Vec::new();
    let take = |path: &Path, index: u32, missing: &mut Vec<char>, out: &mut Vec<Fallback>| {
        if missing.is_empty() || out.len() >= MAX_FALLBACKS || out.iter().any(|f| f.path == path && f.index == index) {
            return;
        }
        let Some((cmap, cff)) = probe_file(path, index) else { return };
        let fb = Fallback { path: path.to_path_buf(), index, name: pdf_name(path, index), cff, cmap, data: OnceLock::new() };
        let before = missing.len();
        missing.retain(|c| !fb.covers(*c));
        if missing.len() < before {
            out.push(fb);
        }
    };
    for (file, index) in preferred(lang) {
        if let Some(path) = by_name.get(file.to_lowercase().as_str()) {
            take(path, index, &mut missing, &mut out);
        }
    }
    // Anything else installed, for scripts still missing.
    let mut swept = 0;
    for (name, path) in &files {
        if missing.is_empty() || swept >= MAX_SWEEP {
            break;
        }
        if SKIP_WORDS.iter().any(|w| name.contains(w)) {
            continue;
        }
        swept += 1;
        take(path, 0, &mut missing, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finding_fonts_never_fails() {
        // Missing and odd folders are skipped; every probe is a font file or nothing.
        let none = discover(&[PathBuf::from("/definitely/not/here")], "ja_JP.UTF-8");
        assert!(none.is_empty());
        let dir = tempfile::tempdir().unwrap();
        for (name, bytes) in [("junk.ttf", &b"not a font"[..]), ("empty.otf", &[][..]), ("ttc.ttc", &b"ttcf\0\0\0\0\xff\xff\xff\xff"[..])] {
            std::fs::write(dir.path().join(name), bytes).unwrap();
        }
        assert!(discover(&[dir.path().to_path_buf()], "").is_empty());
        assert!(probe_file(&dir.path().join("ttc.ttc"), 7).is_none());
    }

    #[test]
    fn the_language_picks_the_cjk_font_first() {
        let first = |lang: &str| preferred(lang).into_iter().nth(3).map(|(f, _)| f);
        assert_eq!(first("ja_JP.UTF-8"), Some("YuGothR.ttc"));
        assert_eq!(first("ko_KR.UTF-8"), Some("malgun.ttf"));
        assert_eq!(first("zh_TW.UTF-8"), Some("msjh.ttc"));
        assert_eq!(first("en_US.UTF-8"), Some("msyh.ttc"));
        assert_eq!(preferred("ja").first(), Some(&("NotoSansCJK-Regular.ttc", 0)));
    }

    #[test]
    fn names_are_safe_in_pdfs() {
        assert_eq!(pdf_name(Path::new("/x/Noto Sans CJK (Regular).ttc"), 2), "NotoSansCJKRegular-2");
        assert_eq!(pdf_name(Path::new("/x/ヒラギノ.ttc"), 0), "Fallback");
    }

    /// On a machine with fonts for these scripts, they are found and cover the probes.
    #[test]
    fn system_fonts_cover_other_scripts_where_installed() {
        let found = discover(&font_dirs(), "en_US.UTF-8");
        for fb in &found {
            assert!(fb.face_data().is_some(), "{} reads", fb.path.display());
        }
        let covered = |c: char| found.iter().any(|f| f.covers(c));
        eprintln!("fallbacks: {:?}", found.iter().map(|f| f.path.display().to_string()).collect::<Vec<_>>());
        eprintln!("漢 {} א {} ก {}", covered('漢'), covered('א'), covered('ก'));
    }
}
