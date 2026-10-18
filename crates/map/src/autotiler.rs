//! Autotiler：把房间的瓦片网格变成每块瓦片在 tileset 纹理里的 `(col, row)`。
//!
//! 算法转录自原版 Celeste `Autotiler.cs`（经 `references/Ruleste` 核对）：
//! 每块固体瓦片探测自己的 3×3 邻居，把邻接情况与 tileset 的掩码列表匹配；
//! 九个邻居全连上时走 `center` / `padding`，否则取第一条匹配的掩码
//! （按通配符数升序，最具体的先）。
//!
//! 变体选择用**位置哈希**而不是 RNG：同一块瓦片永远取同一个变体，
//! 热重载后画面不跳（AGENTS.md §4.3），也不需要流式状态。

use crate::format::Tileset;
use crate::tilesets::{tile_char, TilesetDef, TilesetsDoc};

/// 网格之外（越界）的探测行为，对应原版 `Autotiler.Behaviour.EdgesExtend`
/// （`pureCeleste.cs:87693`）。
///
/// 掩码匹配靠 3×3 邻居，越界怎么算直接决定边缘块取哪张图：
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Oob {
    /// 边缘外延：越界探测钳到边缘格子再读（`GenerateMap` 的房间瓦片行为，
    /// `CheckTile`/`GetTile` 的 `EdgesExtend` 分支，`87986-87996` / `88017-88031`）。
    Extend,
    /// 越界为空：网格之外一律当空（`GenerateBox` 的默认 `Behaviour`
    /// —— `mapData == null` 时返回 `EdgesExtend` = false，`87985-87989`；
    /// `IntroCrusher` 的盒子走的就是它，`106322 → 87832`）。
    Empty,
}

/// 每块瓦片在 tileset 纹理里的坐标（房间瓦片：越界外延）。
///
/// 空白格子的坐标无意义（调用方不会渲染它们）；越界的格子不存在。
pub fn generate(tiles: &Tileset, defs: &TilesetsDoc) -> Vec<(u32, u32)> {
    generate_with(tiles, defs, Oob::Extend)
}

/// [`generate`] 的可配置版：`oob` 选择越界探测行为（见 [`Oob`]）。
pub fn generate_with(tiles: &Tileset, defs: &TilesetsDoc, oob: Oob) -> Vec<(u32, u32)> {
    let mut out = vec![(0u32, 0u32); (tiles.width as usize) * (tiles.height as usize)];

    for ty in 0..tiles.height {
        for tx in 0..tiles.width {
            let value = match tiles.get(tx, ty) {
                Some(v) if v != 0 => v,
                _ => continue, // 0 = 空
            };
            let id = tile_char(value);
            let Some(def) = defs.def(id) else {
                continue; // 未定义的 tile id：调用方负责告警
            };

            let adjacency = adjacency(tiles, tx, ty, def, oob);
            let (col, row) = pick(def, adjacency, tiles, tx, ty, oob);
            out[(ty as usize) * (tiles.width as usize) + (tx as usize)] = (col, row);
        }
    }
    out
}

/// 3×3 邻接数组（`Autotiler.cs:295-321`）。
///
/// 九个探测全部走 `neighbor_connects`，不越界短路 —— 越界怎么处理由
/// `oob` 决定（`Oob::Extend` 钳到边缘格子 = `GenerateMap` 的房间行为；
/// `Oob::Empty` 返回 false = `GenerateBox` 的盒外行为）。
fn adjacency(tiles: &Tileset, tx: u32, ty: u32, def: &TilesetDef, oob: Oob) -> [u8; 9] {
    let offsets: [(i32, i32); 9] = [
        (-1, -1), // TL
        (0, -1),  // T
        (1, -1),  // TR
        (-1, 0),  // L
        (0, 0),   // C
        (1, 0),   // R
        (-1, 1),  // BL
        (0, 1),   // B
        (1, 1),   // BR
    ];
    let mut adjacency = [0u8; 9];
    for (i, (dx, dy)) in offsets.iter().enumerate() {
        if i == 4 {
            adjacency[i] = 1; // 中心对非空瓦片恒为固体
            continue;
        }
        adjacency[i] = u8::from(neighbor_connects(
            tiles,
            tx as i32 + dx,
            ty as i32 + dy,
            def,
            oob,
        ));
    }
    adjacency
}

/// `(x, y)` 处的瓦片是否与 `def` 的 tileset 相连（`CheckTile`）。
///
/// 越界时：`Oob::Empty` 直接不连（`CheckTile` 的 `!EdgesExtend` 分支，
/// `87990-87993`）；`Oob::Extend` 钳到边缘格子再读（`87995-88001`）。
/// 在网格内的空格子不连。
fn neighbor_connects(tiles: &Tileset, x: i32, y: i32, def: &TilesetDef, oob: Oob) -> bool {
    let w = tiles.width as i32;
    let h = tiles.height as i32;
    if (x < 0 || y < 0 || x >= w || y >= h) && oob == Oob::Empty {
        return false;
    }
    let x = x.clamp(0, w - 1);
    let y = y.clamp(0, h - 1);
    let value = tiles.get(x as u32, y as u32).unwrap_or(0);
    if value == 0 {
        return false;
    }
    def.connects(tile_char(value))
}

/// 选 `(col, row)`：全连走 center/padding，否则取第一条匹配的掩码。
fn pick(
    def: &TilesetDef,
    adjacency: [u8; 9],
    tiles: &Tileset,
    tx: u32,
    ty: u32,
    oob: Oob,
) -> (u32, u32) {
    let all_solid = adjacency.iter().all(|&v| v == 1);

    let candidates = if all_solid {
        // 2-away 探测（`Autotiler.cs:316-331`）：任一失败 → padding。
        let far = [(-2i32, 0i32), (2, 0), (0, -2), (0, 2)];
        let padded = far
            .iter()
            .any(|&(dx, dy)| !neighbor_connects(tiles, tx as i32 + dx, ty as i32 + dy, def, oob));
        if padded {
            &def.padding
        } else {
            &def.center
        }
    } else {
        // 第一条匹配的掩码（已按通配符数升序）。
        def.masks
            .iter()
            .find(|m| m.matches(&adjacency))
            .map(|m| &m.tiles)
            .unwrap_or(&def.center)
    };

    if candidates.is_empty() {
        return (0, 0);
    }
    // 位置哈希选变体：确定性，热重载后不跳。
    let index = variant_index(tx, ty, &def.path, candidates.len());
    candidates[index]
}

/// 确定性的变体下标。
fn variant_index(tx: u32, ty: u32, path: &str, len: usize) -> usize {
    // FNV-1a：够散，也够便宜。
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in tx.to_le_bytes().iter().chain(ty.to_le_bytes().iter()) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    for byte in path.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (hash % len as u64) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tilesets::MaskEntry;
    use std::collections::BTreeMap;

    /// 一个只有一条掩码的 tileset：`x0x-111-x1x` → (0,0)，center → (0,2)。
    fn defs() -> TilesetsDoc {
        TilesetsDoc {
            tilesets: BTreeMap::from([(
                "3".into(),
                TilesetDef {
                    path: "tilesets/snow".into(),
                    masks: vec![MaskEntry {
                        mask: [2, 0, 2, 1, 1, 1, 2, 1, 2],
                        tiles: vec![(0, 0)],
                    }],
                    center: vec![(0, 2)],
                    padding: vec![(0, 3)],
                    ignores: vec![],
                },
            )]),
        }
    }

    /// 3×3 全 '3' 的网格。
    fn full_grid() -> Tileset {
        let mut tiles = Tileset::new("Gameplay", 3, 3);
        for y in 0..3 {
            for x in 0..3 {
                tiles.set(x, y, 3);
            }
        }
        tiles
    }

    #[test]
    fn interior_tiles_take_the_center() {
        // 4×4 全 '3'：内部 (1,1)/(2,1)/(1,2)/(2,2) 九邻居全连 → center。
        let mut tiles = Tileset::new("Gameplay", 4, 4);
        for y in 0..4 {
            for x in 0..4 {
                tiles.set(x, y, 3);
            }
        }
        let coords = generate(&tiles, &defs());
        let w = 4usize;
        // 第 1 行第 1 格（row-major：row * w + col）。
        let (col, row) = coords[w + 1];
        assert_eq!((col, row), (0, 2), "interior takes the center variant");
    }

    #[test]
    fn border_tiles_take_a_masked_edge() {
        // 只有底行是固体：三块瓦片的 T 邻居都是空 → x0x-111-x1x 匹配。
        // 角上的左/右邻居越界后钳到自身（EdgesExtend），掩码照样匹配 ——
        // 所以三块都取上边缘变体 (0,0)，与原版一致。
        let mut tiles = Tileset::new("Gameplay", 3, 3);
        for x in 0..3 {
            tiles.set(x, 2, 3); // 只有底行是固体
        }
        let coords = generate(&tiles, &defs());
        for x in 0..3 {
            let (col, row) = coords[2 * 3 + x];
            assert_eq!(
                (col, row),
                (0, 0),
                "tile ({x},2) should take the top-edge variant"
            );
        }
    }

    /// `GenerateBox`（`IntroCrusher` 的盒子）：越界 = 空，所以内块的
    /// 2-away 探测碰到盒外 → padding；房间模式（外延）则全连 → center。
    #[test]
    fn box_mode_treats_outside_the_grid_as_empty() {
        let tiles = full_grid();

        let boxed = generate_with(&tiles, &defs(), Oob::Empty);
        assert_eq!(
            boxed[4],
            (0, 3),
            "center cell: 9 in-grid neighbors connect, but 2-away leaves the box -> padding"
        );
        assert_eq!(
            boxed,
            generate_with(&tiles, &defs(), Oob::Empty),
            "box mode stays deterministic too"
        );

        let extended = generate(&tiles, &defs());
        assert!(
            extended.iter().all(|&c| c == (0, 2)),
            "EdgesExtend (room tiles): every probe clamps to a solid edge cell -> center everywhere"
        );
        assert_ne!(
            boxed[4], extended[4],
            "box mode must not collapse onto the room-mode result"
        );
    }

    #[test]
    fn variant_choice_is_deterministic() {
        let tiles = full_grid();
        let a = generate(&tiles, &defs());
        let b = generate(&tiles, &defs());
        assert_eq!(a, b, "the same grid always picks the same variants");
    }

    #[test]
    fn undefined_tile_ids_are_skipped() {
        let mut tiles = Tileset::new("Gameplay", 2, 1);
        tiles.set(0, 0, 3);
        tiles.set(1, 0, 9); // 没定义 '9'
        let coords = generate(&tiles, &defs());
        assert_eq!(coords[1], (0, 0), "undefined ids stay at (0,0)");
    }

    #[test]
    fn ignores_change_connectivity() {
        // '3' ignores '*'：'3' 不与任何别的 id 连 —— 但同 id 恒连，
        // 所以 3×3 全 '3' 仍然是全连。
        let mut doc = defs();
        doc.tilesets.get_mut("3").unwrap().ignores = vec!['*'];
        let tiles = full_grid();
        let coords = generate(&tiles, &doc);
        let w = 3usize;
        // 第 1 行第 1 格（row-major：row * w + col）。
        let (col, row) = coords[w + 1];
        assert_eq!((col, row), (0, 2), "same id always connects");
    }

    /// golden：真实地图 + 真实 tileset 定义必须产出**多样**的瓦片坐标。
    ///
    /// 这是「所有东西同一张贴图」的直接回归守卫：如果 autotiler 退化成
    /// 全部瓦片取同一个 (col,row)，这个测试就红。数据在 gitignored 的
    /// `assets/` 里，缺了就跳过。
    #[test]
    fn real_map_and_tilesets_produce_varied_tiles() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let tilesets_path = root.join("assets/atlas/tilesets.toml");
        let map_path = root.join("assets/maps/0-Intro.map");
        if !tilesets_path.is_file() || !map_path.is_file() {
            eprintln!("skipping: run the content-pipeline first");
            return;
        }

        let defs = TilesetsDoc::from_file(&tilesets_path).expect("tileset definitions");
        let doc = crate::loader::load_map(&map_path).expect("the intro map");
        let room = &doc.rooms[0];
        let coords = generate(&room.tiles, &defs);

        let mut used = std::collections::HashSet::new();
        for ty in 0..room.tiles.height {
            for tx in 0..room.tiles.width {
                if room.tiles.get(tx, ty).unwrap_or(0) == 0 {
                    continue;
                }
                used.insert(coords[(ty as usize) * (room.tiles.width as usize) + (tx as usize)]);
            }
        }
        assert!(
            used.len() > 2,
            "expected varied tile coordinates, got {used:?} — \
             the autotiler collapsed every tile onto one variant"
        );
    }
}
