//! `Graphics/Sprites.xml` → `atlas/Gameplay.sprites.toml` 转换。
//!
//! 原版 `SpriteBank`（`pureCeleste.cs:15017-15042`）读这个文件：
//! `<Sprites>` 的每个子元素是一个 sprite 定义（`path` = 图集前缀，
//! `start` = 起始动画），子元素 `<Anim id delay frames goto>` /
//! `<Loop id delay frames>` 定义动画（`Loop` = 自循环，`Anim` 可跳转），
//! `frames` 是 CSV（支持 `1-6` 区间，`Calc.ReadCSVIntWithTricks`，
//! 113019 附近同类实现）。帧纹理 = `path + 零填充两位的帧号`
//! （Monocle `GetAtlasSubtextureFromAtlasAt` 的逐级补零查找，14327+；
//! 我们的图集名字就是 `idle00` 这种）。
//!
//! 输出给运行时的是**已解析**的动画表：id → (delay, 帧序号列表, 是否
//! 自循环)。帧名（前缀 + 补零）由运行时解析，这样图集里缺某帧时只
//! 告警不崩（AGENTS.md §15.8/15.9）。

use serde::Serialize;

/// 一条动画定义。
#[derive(Debug, Clone, Serialize)]
pub struct AnimDef {
    /// 动画 id（`Sprite.Play(id)` 的键）。
    pub id: String,
    /// 帧间隔（秒）。0 = 不自动播放（`Animating = Delay > 0f`，10196）。
    pub delay: f32,
    /// 帧序号（对 `path` 前缀的枚举序列取下标）。
    pub frames: Vec<usize>,
    /// `<Anim>` 且带 `goto` → 跳转目标；`<Loop>` → 自循环。
    pub looped: bool,
}

/// 一个 sprite 的动画集。
#[derive(Debug, Clone, Serialize)]
pub struct SpriteDef {
    /// sprite 名（`SpriteBank.SpriteData` 的键，`player` / `badeline`…）。
    pub name: String,
    /// 图集前缀（`characters/player`，正斜杠原样保留 —— 引擎的图集
    /// 查找对分隔符不敏感）。
    pub path: String,
    /// 起始动画（`start` 属性，缺省取第一条）。
    pub start: String,
    pub anims: Vec<AnimDef>,
}

/// 解析失败。
#[derive(Debug, thiserror::Error)]
pub enum SpritesXmlError {
    #[error("unterminated tag at byte {0}")]
    Unterminated(usize),
    #[error("attribute without a value at byte {0}")]
    MalformedAttribute(usize),
}

/// 解析 `Graphics/Sprites.xml` 全部内容。
pub fn parse_sprites(xml: &str) -> Result<Vec<SpriteDef>, SpritesXmlError> {
    let bytes = xml.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    // 跳过 XML 声明/注释，找到 <Sprites>，然后逐个子元素解析。
    while i < bytes.len() {
        if xml[i..].starts_with("<!--") {
            i = skip_to(xml, i, "-->")?;
            continue;
        }
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        // 声明与闭合标签在外层直接跳过（read_tag 只服务开标签 ——
        // 否则 `<?xml ... ?>` 的 `?` 会被当属性名一路吃到下一个 `>`）。
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
        // 顶层 sprite 定义（忽略 <?xml ...?> 之类的伪标签）。
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
            // 子元素扫描，直到本元素的闭合标签。
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
                let frames = cattrs
                    .get("frames")
                    .map(|f| read_csv_int_with_tricks(f))
                    .unwrap_or_default();
                let looped = cname.eq_ignore_ascii_case("Loop") || cattrs.contains_key("goto");
                anims.push(AnimDef {
                    id,
                    delay,
                    frames,
                    looped,
                });
                let _ = cself;
            }
            after_element = j + close.len();
        }
        // 跳过整个元素（含闭合标签），避免子元素被当成顶层定义。
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

/// `Calc.ReadCSVIntWithTricks`：逗号分隔 + `a-b` 闭区间。
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

/// 读一个标签：返回 (名称, 属性表, 是否自闭合, 内容起点/标签终点)。
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
            // 自闭合：/> —— 跳到 '>'
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
            // 无值属性（如 `<Center />` 已在上面处理；这里容忍跳过）。
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
        i += 1; // 跳过引号
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

    /// Anim/Loop/延迟默认值/CSV 区间都要解析出来。
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
        assert_eq!(walk.frames, vec![1, 2, 3, 4, 5, 6]);
        assert_eq!(walk.delay, 0.1, "falls back to the sprite default delay");
        assert!(walk.looped);
        let dash = p.anims.iter().find(|a| a.id == "dash").unwrap();
        assert_eq!(dash.delay, 0.08, "per-anim delay wins");
        assert!(!dash.looped, "<Anim> without goto is one-shot");
        let idle = p.anims.iter().find(|a| a.id == "idle").unwrap();
        assert_eq!(idle.frames, vec![0]);
    }
}
