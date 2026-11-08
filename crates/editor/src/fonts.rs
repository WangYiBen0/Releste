//! CJK font fallback and UI font configuration for the editor.
//!
//! egui ships only Latin/emoji faces, and its built-in proportional font is
//! `Ubuntu-Light` which looks thin on HiDPI/modern displays. AGENTS.md 15.1
//! forbids adding a font crate, so a Chinese catalog would render as tofu and
//! the Latin text would look spindly. This module:
//!
//! 1. Locates a CJK-capable face on the host at run time and registers it as
//!    the lowest-priority fallback for both font families.
//! 2. Optionally applies a visual scale/tweak so glyphs read better; it is
//!    off by default (1.0) and only set through configuration - no automatic
//!    compensation is applied.
//!
//! Nothing is embedded in the binary (fonts are user-provided system
//! infrastructure, like libfmod.so - AGENTS.md 4.7).
//!
//! # Configuration
//!
//! - `RELESTE_UI_FONT=/path/to/font.ttf` forces a specific CJK/fallback file.
//! - `RELESTE_UI_FONT_INDEX=2` picks a face inside a `.ttc` collection.
//! - `RELESTE_UI_FONT_SCALE=1.15` scales the fallback glyphs (visual only;
//!   does not affect layout). Defaults to 1.0 - no compensation.
//! - `RELESTE_UI_FONT_Y_OFFSET=0.0` vertical nudge in points (visual only).
//!
//! The chosen font and UI scale also persist to the settings file managed by
//! [`crate::settings`] (edited in the font settings window); environment
//! variables override that file when set.

use std::path::{Path, PathBuf};

/// Environment variable that forces a specific font file.
pub const FONT_ENV: &str = "RELESTE_UI_FONT";
/// Environment variable that selects a face index inside a collection.
pub const FONT_INDEX_ENV: &str = "RELESTE_UI_FONT_INDEX";
/// Environment variable that scales the fallback font glyphs.
pub const FONT_SCALE_ENV: &str = "RELESTE_UI_FONT_SCALE";
/// Environment variable that nudges the fallback font vertically (points).
pub const FONT_Y_OFFSET_ENV: &str = "RELESTE_UI_FONT_Y_OFFSET";

/// Font name used inside the egui font definitions.
const FALLBACK_NAME: &str = "releste-cjk-fallback";

/// Default scale for the CJK fallback: no scaling at all. Pick a static font
/// with real Regular faces (see [`font_candidates`]) instead of compensating.
pub const DEFAULT_SCALE: f32 = 1.0;

/// Lowercase filename fragments that identify a CJK-capable face.
/// Ordered by preference: static Source Han Sans ships discrete Regular
/// faces (epaint 0.31 cannot set variable-font axes, so VF fonts render at
/// their default instance, which is Thin for Noto Sans CJK VF).
const NAME_HINTS: &[&str] = &[
    "sourcehansans",
    "source-han-sans",
    "notosanscjk",
    "notosanssc",
    "sarasa",
    "wqy",
    "wenquanyi",
    "droidsansfallback",
    "msyh",
    "simhei",
    "simsun",
    "pingfang",
    "hiragino",
];

/// Resolved font configuration.
#[derive(Debug, Clone)]
pub struct FontConfig {
    pub path: Option<PathBuf>,
    pub index: u32,
    pub scale: f32,
    pub y_offset: f32,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

impl FontConfig {
    /// Reads configuration from environment variables.
    pub fn from_env() -> Self {
        Self {
            path: explicit_font_path(),
            index: parse_face_index(std::env::var(FONT_INDEX_ENV).ok().as_deref()),
            scale: parse_f32_env(FONT_SCALE_ENV, DEFAULT_SCALE),
            y_offset: parse_f32_env(FONT_Y_OFFSET_ENV, 0.0),
        }
    }
}

fn explicit_font_path() -> Option<PathBuf> {
    let explicit = std::env::var(FONT_ENV).ok()?;
    if explicit.is_empty() {
        return None;
    }
    let path = PathBuf::from(&explicit);
    if path.is_file() {
        Some(path)
    } else {
        tracing::warn!(path = %explicit, "RELESTE_UI_FONT does not name a readable file");
        None
    }
}

fn parse_f32_env(name: &str, default: f32) -> f32 {
    match std::env::var(name) {
        Ok(v) => v.trim().parse::<f32>().unwrap_or_else(|_| {
            tracing::warn!(name, value = %v, "invalid float; using default");
            default
        }),
        Err(_) => default,
    }
}

/// Installs the fallback on `ctx` using the default (env) configuration.
/// Returns the font path that was used, if any.
pub fn install_cjk_fallback(ctx: &egui::Context) -> Option<PathBuf> {
    install_cjk_fallback_with(ctx, &FontConfig::default())
}

/// Installs the fallback with an explicit configuration.
pub fn install_cjk_fallback_with(ctx: &egui::Context, config: &FontConfig) -> Option<PathBuf> {
    let path = config.path.clone().or_else(find_cjk_font)?;
    // An explicit selection (settings window or RELESTE_UI_FONT) honours the
    // stored face index; the auto-detected path gets its preferred face.
    let index = if config.path.is_some() {
        config.index
    } else {
        preferred_face_index(&path)
    };
    let scale = if config.scale.is_finite() && config.scale > 0.0 {
        config.scale
    } else {
        DEFAULT_SCALE
    };
    let y_offset = if config.y_offset.is_finite() {
        config.y_offset
    } else {
        0.0
    };

    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "cannot read UI font");
            return None;
        }
    };

    let mut data = egui::FontData::from_owned(bytes);
    data.index = index;
    // Visual-only tweak: scales glyphs and nudges baseline without affecting
    // text layout. This compensates for thin variable-font default instances
    // (e.g. Noto Sans CJK VF defaults to wght=100 Thin) and the general
    // "too thin" feedback.
    data.tweak = egui::FontTweak {
        scale,
        y_offset_factor: 0.0,
        y_offset,
        baseline_offset_factor: 0.0,
    };

    ctx.add_font(egui::epaint::text::FontInsert::new(
        FALLBACK_NAME,
        data,
        vec![
            egui::epaint::text::InsertFontFamily {
                family: egui::FontFamily::Proportional,
                priority: egui::epaint::text::FontPriority::Lowest,
            },
            egui::epaint::text::InsertFontFamily {
                family: egui::FontFamily::Monospace,
                priority: egui::epaint::text::FontPriority::Lowest,
            },
        ],
    ));

    tracing::info!(
        path = %path.display(),
        index,
        scale,
        y_offset,
        "CJK fallback font installed"
    );
    Some(path)
}

/// Reports that no CJK font was found, with an actionable message.
pub fn warn_missing_cjk_font() {
    tracing::warn!(
        "no CJK font found: Chinese labels will render as boxes. \
         Fix: install a CJK font (Noto Sans CJK, Source Han Sans, WenQuanYi) or set \
         {FONT_ENV}=/path/to/font.ttf"
    );
}

/// Picks a readable default face index for known font collections.
/// epaint 0.31 has no variable-font axis control, so VF collections may
/// default to Thin; static collections may start at ExtraLight.
pub fn preferred_face_index(path: &Path) -> u32 {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if name.contains("sourcehansans") || name.contains("source-han-sans") {
        // Source Han Sans TTC: [ExtraLight,Light,Normal,Regular,Medium,Bold,Heavy]
        // x [JP,KR,SC,TC,HK]. SC Regular = 2*7 + 3 = 17.
        return 17;
    }
    0
}

/// Parses the face-index override; anything unparseable means face 0.
fn parse_face_index(raw: Option<&str>) -> u32 {
    raw.and_then(|v| v.trim().parse::<u32>().ok()).unwrap_or(0)
}

/// Finds a CJK-capable font file, honouring `RELESTE_UI_FONT` first.
pub fn find_cjk_font() -> Option<PathBuf> {
    let mut best: Option<(usize, PathBuf)> = None;
    for dir in candidate_dirs() {
        for file in walk(&dir, 0) {
            let Some(rank) = rank(&file) else { continue };
            let better = match &best {
                Some((best_rank, _)) => rank < *best_rank,
                None => true,
            };
            if better {
                best = Some((rank, file));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// One selectable fallback font found on this system.
#[derive(Debug, Clone)]
pub struct FontCandidate {
    /// Absolute path of the font file.
    pub path: PathBuf,
    /// Number of faces inside the file (1 for plain ttf/otf).
    pub faces: u32,
}

/// Lists CJK-capable font files for the settings window, best first.
///
/// `fc-list` is authoritative where available (it also sees fonts linked
/// through nix profiles that a plain directory walk misses); the directory
/// walk is the fallback. Results are sorted by [`rank`] and capped so a
/// pathological system cannot flood the picker.
pub fn font_candidates() -> Vec<FontCandidate> {
    let mut files: Vec<PathBuf> = fc_list_fonts().unwrap_or_default();
    if files.is_empty() {
        for dir in candidate_dirs() {
            files.extend(walk(&dir, 0));
        }
    }
    let mut ranked: Vec<(usize, PathBuf)> = files
        .into_iter()
        .filter_map(|p| rank(&p).map(|r| (r, p)))
        .collect();
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    ranked.dedup_by(|a, b| a.1 == b.1);
    ranked.truncate(64);
    ranked
        .into_iter()
        .map(|(_, p)| FontCandidate {
            faces: face_count(&p),
            path: p,
        })
        .collect()
}

/// Queries fontconfig for every installed font file on this host.
fn fc_list_fonts() -> Option<Vec<PathBuf>> {
    let output = std::process::Command::new("fc-list")
        .arg("--format=%{file}\n")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    Some(
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .collect(),
    )
}

/// Number of faces inside a font file: parses the `.ttc` header; anything
/// else counts as a single face.
pub fn face_count(path: &Path) -> u32 {
    use std::io::Read;
    let mut head = [0u8; 12];
    let Ok(mut file) = std::fs::File::open(path) else {
        return 1;
    };
    if file.read_exact(&mut head).is_err() {
        return 1;
    }
    if &head[0..4] == b"ttcf" {
        u32::from_be_bytes([head[8], head[9], head[10], head[11]])
    } else {
        1
    }
}

/// Ranks a candidate file by how well its name matches a preferred face.
/// Lower is better; `None` means it is not a CJK font.
fn rank(path: &Path) -> Option<usize> {
    if !has_font_extension(path) {
        return None;
    }
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    NAME_HINTS.iter().position(|hint| name.contains(hint))
}

fn has_font_extension(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("ttf" | "ttc" | "otf")
    )
}

/// Directories that commonly hold system fonts, in preference order.
fn candidate_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for var in ["XDG_DATA_HOME", "HOME"] {
        if let Ok(base) = std::env::var(var) {
            if !base.is_empty() {
                dirs.push(PathBuf::from(&base).join(".local/share/fonts"));
                dirs.push(PathBuf::from(&base).join(".nix-profile/share/fonts"));
            }
        }
    }
    if let Ok(user) = std::env::var("USER") {
        dirs.push(
            PathBuf::from("/etc/profiles/per-user")
                .join(user)
                .join("share/fonts"),
        );
    }
    dirs.push(PathBuf::from("/run/current-system/sw/share/fonts"));
    dirs.push(PathBuf::from("/usr/share/fonts"));
    dirs.push(PathBuf::from("/usr/local/share/fonts"));
    dirs.push(PathBuf::from("/System/Library/Fonts"));
    dirs.push(PathBuf::from("/Library/Fonts"));
    dirs.push(PathBuf::from("C:/Windows/Fonts"));
    dirs
}

/// Recursively lists font files, bounded so a huge tree cannot stall startup.
fn walk(dir: &Path, depth: usize) -> Vec<PathBuf> {
    const MAX_DEPTH: usize = 4;
    let mut out = Vec::new();
    if depth > MAX_DEPTH {
        return out;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path, depth + 1));
        } else if has_font_extension(&path) {
            out.push(path);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_cjk_face_names_and_rejects_others() {
        assert!(rank(Path::new("/f/NotoSansCJK-Regular.ttc")).is_some());
        assert!(rank(Path::new("/f/SourceHanSans.ttc")).is_some());
        assert!(rank(Path::new("/f/wqy-microhei.ttc")).is_some());
        assert_eq!(rank(Path::new("/f/DejaVuSans.ttf")), None);
        assert_eq!(rank(Path::new("/f/NotoSansCJK.txt")), None);
    }

    #[test]
    fn noto_sans_cjk_is_preferred_over_wenquanyi() {
        let a = rank(Path::new("/f/NotoSansCJK.ttc")).unwrap();
        let b = rank(Path::new("/f/wqy-zenhei.ttc")).unwrap();
        assert!(a < b, "Noto Sans CJK is preferred over WenQuanYi");
    }

    #[test]
    fn face_index_defaults_to_zero_and_honours_the_override() {
        assert_eq!(parse_face_index(Some("2")), 2);
        assert_eq!(parse_face_index(Some(" 3 ")), 3);
        assert_eq!(parse_face_index(Some("")), 0);
        assert_eq!(parse_face_index(Some("not-a-number")), 0);
        assert_eq!(parse_face_index(None), 0);
    }

    #[test]
    fn font_scale_env_parses_and_falls_back_safely() {
        assert_eq!(
            parse_f32_env("__RELESTE_FONT_SCALE_NONEXISTENT__", 1.12),
            1.12
        );
    }

    #[test]
    fn walking_a_missing_directory_yields_nothing() {
        assert!(walk(Path::new("/nonexistent/releste/fonts"), 0).is_empty());
    }

    #[test]
    fn face_counts_a_synthetic_collection_header() {
        // CARGO_TARGET_TMPDIR only exists for integration tests, so this unit
        // test writes its synthetic header under the workspace target dir.
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp-font-count");
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        let path = dir.join("synthetic.ttc");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"ttcf");
        bytes.extend_from_slice(&[0, 1, 0, 0]); // version
        bytes.extend_from_slice(&3u32.to_be_bytes()); // numFonts
        std::fs::write(&path, &bytes).expect("write synthetic ttc");
        assert_eq!(face_count(&path), 3);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn candidates_are_font_files_sorted_by_rank() {
        let candidates = font_candidates();
        if candidates.is_empty() {
            eprintln!("skipping: no CJK fonts on this host");
            return;
        }
        for candidate in &candidates {
            assert!(
                candidate.path.is_file(),
                "candidate {} is not a file",
                candidate.path.display()
            );
            assert!(
                candidate.faces >= 1,
                "candidate {} has no faces",
                candidate.path.display()
            );
        }
        for window in candidates.windows(2) {
            let a = rank(&window[0].path).expect("ranked candidate");
            let b = rank(&window[1].path).expect("ranked candidate");
            assert!(a <= b, "candidates not sorted by rank");
        }
    }

    #[test]
    fn installed_fallback_can_render_han_characters() {
        let ctx = egui::Context::default();
        let Some(path) = install_cjk_fallback(&ctx) else {
            eprintln!("skipping: no CJK font on this host");
            return;
        };
        let han = "\u{623f}\u{95f4}";
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        let proportional = ctx.fonts(|f| f.has_glyphs(&egui::FontId::proportional(14.0), han));
        let monospace = ctx.fonts(|f| f.has_glyphs(&egui::FontId::monospace(14.0), han));
        assert!(
            proportional,
            "proportional UI font {} missing Han",
            path.display()
        );
        assert!(
            monospace,
            "monospace UI font {} missing Han",
            path.display()
        );
        let latin = ctx.fonts(|f| f.has_glyphs(&egui::FontId::proportional(14.0), "File"));
        assert!(latin, "installing the fallback must not drop Latin glyphs");
    }
}
