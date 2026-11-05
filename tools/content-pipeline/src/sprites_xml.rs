//! `Graphics/Sprites.xml` -> `atlas/Gameplay.sprites.toml` conversion.
//!
//! Vanilla `SpriteBank` (`pureCeleste.cs:15017-15042`) reads this file:
//! every child element of `<Sprites>` is a sprite definition (`path` = atlas prefix,
//! `start` = starting animation); the child elements `<Anim id delay frames goto>` /
//! `<Loop id delay frames>` define animations (`Loop` = self-loop, `Anim` can jump),
//! `frames` is CSV (supports `1-6` ranges, `Calc.ReadCSVIntWithTricks`,
//! implemented nearby at 113019). A frame texture = `path + two-digit zero-padded frame number`
//! (Monocle `GetAtlasSubtextureFromAtlasAt`'s stepwise zero-padded lookup, 14327+;
//! our atlas names are exactly things like `idle00`).
//!
//! What is handed to the runtime is the **already parsed** animation table: id -> (delay, frame index list, whether
//! it self-loops). Frame names (prefix + zero padding) are resolved by the runtime, so a missing frame in the atlas only
//! warns instead of crashing (AGENTS.md 15.8/15.9).

use serde::Serialize;

/// One animation definition.
#[derive(Debug, Clone, Serialize)]
pub struct AnimDef {
    /// Animation id (the key for `Sprite.Play(id)`).
    pub id: String,
    /// Frame interval (seconds). 0 = do not auto-play (`Animating = Delay > 0f`, 10196).
    pub delay: f32,
    /// Frame indices (subscripts into the enumerated sequence of the `path` prefix). `None` = attribute absent,
    /// take **all** frames (Monocle `GetFrames(path, null)` enumerates in order until a frame is missing,
    /// pureCeleste.cs:10451-10464) -- the runtime expands them against the atlas.
    pub frames: Option<Vec<usize>>,
    /// The jump target of `<Anim goto="...">`; `<Loop>` = self-loop (own id);
    /// `None` = stop on the last frame after playing (10296-10305).
    pub goto: Option<String>,
    /// Animation sub-path (`<Anim path="idle">`): frame name = the sprite's `path` +
    /// this + zero-padded index (the `Path + path` concatenation in `GetFrames`,
    /// pureCeleste.cs:10454-10462). Absent = "".
    pub path: String,
}

/// The animation set of one sprite.
#[derive(Debug, Clone, Serialize)]
pub struct SpriteDef {
    /// sprite name (the key of `SpriteBank.SpriteData`, `player` / `badeline`...).
    pub name: String,
    /// Atlas prefix (`characters/player`; forward slashes are kept as-is -- the engine's atlas
    /// lookup is insensitive to separators).
    pub path: String,
    /// Starting animation (the `start` attribute; defaults to the first one).
    pub start: String,
    pub anims: Vec<AnimDef>,
}

/// Parse failure.
#[derive(Debug, thiserror::Error)]
pub enum SpritesXmlError {
    #[error("unterminated tag at byte {0}")]
    Unterminated(usize),
    #[error("attribute without a value at byte {0}")]
    MalformedAttribute(usize),
}

/// Parse all content of `Graphics/Sprites.xml`.
///
/// The real file starts with a UTF-8 BOM (`\u{feff}`) -- strip it first, otherwise the byte scanner
/// slices at a non-character boundary (panic).
pub fn parse_sprites(xml: &str) -> Result<Vec<SpriteDef>, SpritesXmlError> {
    let xml = xml.strip_prefix('\u{feff}').unwrap_or(xml);
    let bytes = xml.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    // Skip the XML declaration/comments, find <Sprites>, then parse each child element.
    while i < bytes.len() {
        if xml[i..].starts_with("<!--") {
            i = skip_to(xml, i, "-->")?;
            continue;
        }
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        // Declarations and closing tags are skipped at the outer level (read_tag only serves open tags --
        // otherwise the `?` in `<?xml ... ?>` would be read as an attribute name all the way to the next `>`).
        if xml[i..].starts_with("<?") {
            i = skip_to(xml, i, "?>")?;
            continue;
        }
        if xml[i..].starts_with("</") {
            i = skip_to(xml, i, ">")?;
            continue;
        }
        let (name, attrs, self_closing, end) = read_tag(xml, i)?;
        if name == "Sprites" {
            i = end;
            continue;
        }
        if name.eq_ignore_ascii_case("Sprites") {
            i = end;
            continue;
        }
        // Top-level sprite definition (ignore pseudo-tags like <?xml ...?>).
        if name.contains('?') || name.starts_with('!') || name.starts_with('/') {
            i = end;
            continue;
        }
        let path = attrs.get("path").cloned().unwrap_or_default();
        let start = attrs.get("start").cloned().unwrap_or_default();
        let default_delay: f32 = attrs
            .get("delay")
            .and_then(|d| d.parse().ok())
            .unwrap_or(0.0);
        let mut anims = Vec::new();
        let mut after_element = end;
        if !self_closing {
            // Child scan until this element's closing tag.
            let close = format!("</{name}>");
            let mut j = end;
            while j < bytes.len() && !xml[j..].starts_with(&close) {
                if xml[j..].starts_with("<!--") {
                    j = skip_to(xml, j, "-->")?;
                    continue;
                }
                if bytes[j] != b'<' {
                    j += 1;
                    continue;
                }
                let (cname, cattrs, cself, cend) = read_tag(xml, j)?;
                j = cend;
                let id = match cattrs.get("id") {
                    Some(id) => id.clone(),
                    None => continue,
                };
                let delay: f32 = cattrs
                    .get("delay")
                    .and_then(|d| d.parse().ok())
                    .unwrap_or(default_delay);
                // `<Loop>` = self-loop (Monocle `AddLoop`:
                // `Goto = new Chooser<string>(id, 1f)`, 10338-10345);
                // `<Anim goto="...">` = jump to the target animation.
                let goto = if cname.eq_ignore_ascii_case("Loop") {
                    Some(id.clone())
                } else {
                    cattrs.get("goto").cloned()
                };
                let frames = cattrs.get("frames").map(|f| read_csv_int_with_tricks(f));
                let anim_path = cattrs.get("path").cloned().unwrap_or_default();
                anims.push(AnimDef {
                    id,
                    delay,
                    frames,
                    goto,
                    path: anim_path,
                });
                let _ = cself;
            }
            after_element = j + close.len();
        }
        // Skip the whole element (including its closing tag) so children are not mistaken for top-level definitions.
        i = after_element;
        let start = if start.is_empty() {
            anims.first().map(|a| a.id.clone()).unwrap_or_default()
        } else {
            start
        };
        out.push(SpriteDef {
            name,
            path,
            start,
            anims,
        });
    }
    Ok(out)
}

/// `Calc.ReadCSVIntWithTricks`: comma-separated + `a-b` inclusive ranges.
pub fn read_csv_int_with_tricks(list: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for piece in list.split(',') {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        if let Some((a, b)) = piece.split_once('-') {
            if let (Ok(a), Ok(b)) = (a.trim().parse::<i64>(), b.trim().parse::<i64>()) {
                let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                out.extend((lo..=hi).map(|v| v.max(0) as usize));
                continue;
            }
        }
        if let Ok(v) = piece.parse::<i64>() {
            if v >= 0 {
                out.push(v as usize);
            }
        }
    }
    out
}

/// Read one tag: returns (name, attribute table, whether self-closing, content start/tag end).
fn read_tag(
    xml: &str,
    start: usize,
) -> Result<(String, HashMap<String, String>, bool, usize), SpritesXmlError> {
    let bytes = xml.as_bytes();
    let mut i = start + 1;
    let name_start = i;
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' && bytes[i] != b'/'
    {
        i += 1;
    }
    let name = xml[name_start..i].trim().to_string();
    let mut attrs = HashMap::new();
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            return Err(SpritesXmlError::Unterminated(start));
        }
        if bytes[i] == b'/' {
            // Self-closing: /> -- jump to '>'
            while i < bytes.len() && bytes[i] != b'>' {
                i += 1;
            }
            return Ok((name, attrs, true, i + 1));
        }
        if bytes[i] == b'>' {
            return Ok((name, attrs, false, i + 1));
        }
        let key_start = i;
        while i < bytes.len() && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let key = xml[key_start..i].to_string();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            // Valueless attribute (like `<Center />` handled above; tolerated and skipped here).
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || (bytes[i] != b'"' && bytes[i] != b'\'') {
            return Err(SpritesXmlError::MalformedAttribute(i));
        }
        let quote = bytes[i];
        i += 1;
        let val_start = i;
        while i < bytes.len() && bytes[i] != quote {
            i += 1;
        }
        attrs.insert(key, xml[val_start..i].to_string());
        i += 1; // skip the quote
    }
}

fn skip_to(xml: &str, start: usize, marker: &str) -> Result<usize, SpritesXmlError> {
    match xml[start..].find(marker) {
        Some(pos) => Ok(start + pos + marker.len()),
        None => Err(SpritesXmlError::Unterminated(start)),
    }
}

use std::collections::HashMap;

#[cfg(test)]
mod tests {
    use super::*;

    /// Anim/Loop/default delay/CSV ranges must all be parsed out.
    #[test]
    fn parses_anim_loop_and_csv_ranges() {
        let xml = r#"<?xml version="1.0"?>
<Sprites>
  <player path="characters/player" start="idle" delay="0.1">
    <Center />
    <Loop id="idle" frames="0" />
    <Loop id="walk" frames="1-6" />
    <Anim id="dash" delay="0.08" frames="0,1,2" />
  </player>
</Sprites>
"#;
        let defs = parse_sprites(xml).unwrap();
        assert_eq!(defs.len(), 1);
        let p = &defs[0];
        assert_eq!(p.name, "player");
        assert_eq!(p.path, "characters/player");
        assert_eq!(p.start, "idle");
        assert_eq!(p.anims.len(), 3);
        let walk = p.anims.iter().find(|a| a.id == "walk").unwrap();
        assert_eq!(walk.frames, Some(vec![1, 2, 3, 4, 5, 6]));
        assert_eq!(walk.delay, 0.1, "falls back to the sprite default delay");
        assert_eq!(walk.goto.as_deref(), Some("walk"), "<Loop> self-loops");
        let dash = p.anims.iter().find(|a| a.id == "dash").unwrap();
        assert_eq!(dash.delay, 0.08, "per-anim delay wins");
        assert_eq!(dash.goto, None, "<Anim> without goto is one-shot");
        let idle = p.anims.iter().find(|a| a.id == "idle").unwrap();
        assert_eq!(idle.frames, Some(vec![0]));
    }
}
