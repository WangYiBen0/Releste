//! `ForegroundTiles.xml` → 引擎原生 [`TilesetsDoc`]。
//!
//! XML 是原始格式，只在 build time 解析（AGENTS.md §4.6）；产物是
//! `atlas/tilesets.toml`，运行时由 `releste_map::TilesetsDoc` 读取。
//!
//! `copy="z"` 的 tileset 继承被拷贝者的 masks / center / padding
//! （原版 `Autotiler` 对 tileset 自己的 set 调一次 `ReadInto`，
//! 再对被拷贝者的 set 调一次）。

use std::collections::BTreeMap;
use std::path::Path;

use releste_map::{MaskEntry, TilesetDef, TilesetsDoc};

use crate::xml::{parse_tilesets, XmlError};

/// 解析 XML 文本 → 全部 tileset 定义（含 `copy` 继承）。
pub fn from_xml(xml: &str) -> Result<TilesetsDoc, XmlError> {
    let elements = parse_tilesets(xml)?;

    // 先按出现顺序展开；`copy` 需要查前面已定义的 tileset。
    let mut tilesets: BTreeMap<String, TilesetDef> = BTreeMap::new();
    for element in elements {
        let id = element.id;
        let mut masks: Vec<MaskEntry> = element
            .sets
            .iter()
            .filter(|set| set.mask != "center" && set.mask != "padding")
            .map(|set| MaskEntry {
                mask: parse_mask(&set.mask),
                tiles: parse_tiles(&set.tiles),
            })
            .collect();
        let center = special_tiles(&element.sets, "center");
        let padding = special_tiles(&element.sets, "padding");

        // `copy="z"`：继承被拷贝者的 masks / center / padding。
        if let Some(copy_id) = element.copy.as_deref() {
            if let Some(parent) = tilesets.get(copy_id) {
                masks.extend(parent.masks.iter().cloned());
            }
        }
        let center = if center.is_empty() {
            element
                .copy
                .as_deref()
                .and_then(|c| tilesets.get(c))
                .map(|p| p.center.clone())
                .unwrap_or_default()
        } else {
            center
        };
        let padding = if padding.is_empty() {
            element
                .copy
                .as_deref()
                .and_then(|c| tilesets.get(c))
                .map(|p| p.padding.clone())
                .unwrap_or_default()
        } else {
            padding
        };

        // 按通配符数升序（最具体的先匹配），对应原版 `Masked.Sort`。
        masks.sort_by_key(|m| m.wildcards());

        let ignores = element
            .ignores
            .as_deref()
            .unwrap_or("")
            .split(',')
            .filter_map(|s| s.trim().chars().next())
            .collect();

        tilesets.insert(
            id,
            TilesetDef {
                // XML 的 `path="snow"` 是相对 `tilesets/` 的名字；存成
                // 图集内完整 sprite 路径，运行时直接查。
                path: element
                    .path
                    .map(|p| format!("tilesets/{p}"))
                    .unwrap_or_else(|| "template".to_string()),
                masks,
                center,
                padding,
                ignores,
            },
        );
    }

    Ok(TilesetsDoc { tilesets })
}

/// 从 XML 文件读取并转换。
pub fn from_file(path: &Path) -> anyhow::Result<TilesetsDoc> {
    let xml = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
    // 去掉 BOM（Celeste 的 XML 带 EF BB BF）。
    let xml = xml.strip_prefix('\u{feff}').unwrap_or(&xml);
    from_xml(xml).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
}

/// `center` / `padding` 这类特殊 set 的 tiles。
fn special_tiles(sets: &[crate::xml::SetElement], name: &str) -> Vec<(u32, u32)> {
    sets.iter()
        .find(|set| set.mask == name)
        .map(|set| parse_tiles(&set.tiles))
        .unwrap_or_default()
}

/// `"x0x-111-x1x"` → `[2,0,2,1,1,1,2,1,2]`。
///
/// 布局 `[TL, T, TR, L, C, R, BL, B, BR]`：`0`=空 `1`=固体 `x`=通配。
fn parse_mask(mask: &str) -> [u8; 9] {
    let mut out = [0u8; 9];
    let chars: Vec<char> = mask.chars().filter(|c| *c != '-').collect();
    for (i, ch) in chars.iter().enumerate().take(9) {
        out[i] = match ch {
            '0' => 0,
            '1' => 1,
            'x' | 'X' => 2,
            _ => continue,
        };
    }
    out
}

/// `"0,0;1,0;2,0"` → `[(0,0), (1,0), (2,0)]`。
fn parse_tiles(tiles: &str) -> Vec<(u32, u32)> {
    tiles
        .split(';')
        .filter_map(|pair| {
            let mut parts = pair.trim().splitn(2, ',');
            let col = parts.next()?.trim().parse::<u32>().ok()?;
            let row = parts.next()?.trim().parse::<u32>().ok()?;
            Some((col, row))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_inherits_masks_center_and_padding() {
        let xml = r#"<Data>
  <Tileset id="z" path="template">
    <set mask="x0x-111-x1x" tiles="0,0;1,0"/>
    <set mask="center" tiles="4,0"/>
    <set mask="padding" tiles="5,0"/>
  </Tileset>
  <Tileset id="3" copy="z" path="snow"/>
</Data>"#;
        let doc = from_xml(xml).expect("parses");
        let snow = doc.def('3').expect("snow defined");
        assert_eq!(snow.path, "tilesets/snow");
        assert_eq!(snow.masks.len(), 1, "inherits the template mask");
        assert_eq!(snow.center, vec![(4, 0)]);
        assert_eq!(snow.padding, vec![(5, 0)]);
    }

    #[test]
    fn masks_are_sorted_by_wildcard_count() {
        // 通配多的排后面：x1x-111-x1x（3 个通配）在 111-111-110（0 个）之后。
        let xml = r#"<Data>
  <Tileset id="z" path="template">
    <set mask="x1x-111-x1x" tiles="0,0"/>
    <set mask="111-111-110" tiles="4,0"/>
  </Tileset>
</Data>"#;
        let doc = from_xml(xml).expect("parses");
        let def = doc.def('z').expect("defined");
        assert_eq!(def.masks[0].mask, [1, 1, 1, 1, 1, 1, 1, 1, 0]);
        assert_eq!(def.masks[1].mask, [2, 1, 2, 1, 1, 1, 2, 1, 2]);
    }

    #[test]
    fn ignores_are_parsed() {
        let xml = r#"<Data><Tileset id="1" copy="z" path="dirt" ignores="g,*"/></Data>"#;
        let doc = from_xml(xml).expect("parses");
        let dirt = doc.def('1').expect("defined");
        assert_eq!(dirt.ignores, vec!['g', '*']);
    }

    #[test]
    fn real_file_parses_if_present() {
        // golden：钉住真实文件（在 gitignored 的 references/ 里，缺了就跳过）。
        let path = std::path::Path::new("references/Celeste/Content/Graphics/ForegroundTiles.xml");
        if !path.is_file() {
            eprintln!("skipping: {path:?} is not present");
            return;
        }
        let doc = from_file(path).expect("parses the real file");
        let snow = doc.def('3').expect("tileset 3 (snow) defined");
        assert_eq!(snow.path, "tilesets/snow");
        assert!(!snow.masks.is_empty(), "snow inherits the template masks");
        let dirt = doc.def('1').expect("tileset 1 (dirt) defined");
        assert_eq!(dirt.ignores, vec!['g']);
    }
}
