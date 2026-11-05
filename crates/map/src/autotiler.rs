//! Autotiler: turn a room's tile grid into each tile's `(col, row)` in the tileset texture.
//!
//! The algorithm is transcribed from vanilla Celeste `Autotiler.cs` (checked against `references/Ruleste`):
//! every solid tile probes its own 3x3 neighborhood and matches the adjacency pattern against
//! the tileset's mask list; when all nine neighbors connect it uses `center` / `padding`,
//! otherwise it takes the first matching mask (ascending wildcard count, most specific first).
//!
//! Variant selection uses a **position hash** rather than an RNG: the same tile always picks the
//! same variant, so the picture does not jump after a hot reload (AGENTS.md 4.3), and no streaming state is needed.

use crate::format::Tileset;
use crate::tilesets::{tile_char, TilesetDef, TilesetsDoc};

/// Out-of-grid (out-of-bounds) probe behavior, corresponding to vanilla `Autotiler.Behaviour.EdgesExtend`
/// (`pureCeleste.cs:87693`).
///
/// Mask matching relies on the 3x3 neighborhood, so how out-of-bounds is treated decides which tile an edge cell gets:
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Oob {
    /// Edge extension: an out-of-bounds probe clamps to the edge cell and reads it (the room-tile behavior of `GenerateMap`,
    /// the `EdgesExtend` branch of `CheckTile`/`GetTile`, `87986-87996` / `88017-88031`).
    Extend,
    /// Out-of-bounds is empty: anything outside the grid counts as empty (the default `Behaviour` of `GenerateBox`
    /// -- it returns `EdgesExtend` = false when `mapData == null`, `87985-87989`;
    /// the `IntroCrusher` box uses exactly this, `106322 -> 87832`).
    Empty,
}

/// Each tile's coordinate in the tileset texture (room tiles: out-of-bounds extends).
///
/// The coordinate of a blank cell is meaningless (callers do not render them); out-of-bounds cells do not exist.
pub fn generate(tiles: &Tileset, defs: &TilesetsDoc) -> Vec<(u32, u32)> {
    generate_with(tiles, defs, Oob::Extend)
}

/// Configurable version of [`generate`]: `oob` selects the out-of-bounds probe behavior (see [`Oob`]).
pub fn generate_with(tiles: &Tileset, defs: &TilesetsDoc, oob: Oob) -> Vec<(u32, u32)> {
    let mut out = vec![(0u32, 0u32); (tiles.width as usize) * (tiles.height as usize)];

    for ty in 0..tiles.height {
        for tx in 0..tiles.width {
            let value = match tiles.get(tx, ty) {
                Some(v) if v != 0 => v,
                _ => continue, // 0 = empty
            };
            let id = tile_char(value);
            let Some(def) = defs.def(id) else {
                continue; // undefined tile id: the caller is responsible for warning
            };

            let adjacency = adjacency(tiles, tx, ty, def, oob);
            let (col, row) = pick(def, adjacency, tiles, tx, ty, oob);
            out[(ty as usize) * (tiles.width as usize) + (tx as usize)] = (col, row);
        }
    }
    out
}

/// 3x3 adjacency array (`Autotiler.cs:295-321`).
///
/// All nine probes go through `neighbor_connects` with no out-of-bounds short-circuit -- how out-of-bounds
/// is decided by `oob` (`Oob::Extend` clamps to the edge cell = the room behavior of `GenerateMap`;
/// `Oob::Empty` returns false = the outside-the-box behavior of `GenerateBox`).
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
            adjacency[i] = 1; // the center is always solid for a non-empty tile
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

/// Whether the tile at `(x, y)` connects to `def`'s tileset (`CheckTile`).
///
/// Out of bounds: `Oob::Empty` simply does not connect (the `!EdgesExtend` branch of `CheckTile`,
/// `87990-87993`); `Oob::Extend` clamps to the edge cell and reads it (`87995-88001`).
/// An empty cell inside the grid does not connect.
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

/// Pick `(col, row)`: all-connected uses center/padding, otherwise the first matching mask.
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
        // 2-away probe (`Autotiler.cs:316-331`): any failure -> padding.
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
        // The first matching mask (already sorted ascending by wildcard count).
        def.masks
            .iter()
            .find(|m| m.matches(&adjacency))
            .map(|m| &m.tiles)
            .unwrap_or(&def.center)
    };

    if candidates.is_empty() {
        return (0, 0);
    }
    // Position hash picks the variant: deterministic, no jump after hot reload.
    let index = variant_index(tx, ty, &def.path, candidates.len());
    candidates[index]
}

/// Deterministic variant index.
fn variant_index(tx: u32, ty: u32, path: &str, len: usize) -> usize {
    // FNV-1a: spread enough, and cheap enough.
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

    /// A tileset with a single mask: `x0x-111-x1x` -> (0,0), center -> (0,2).
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

    /// A 3x3 grid that is all '3'.
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
        // 4x4 all '3': the interior (1,1)/(2,1)/(1,2)/(2,2) has all nine neighbors connected -> center.
        let mut tiles = Tileset::new("Gameplay", 4, 4);
        for y in 0..4 {
            for x in 0..4 {
                tiles.set(x, y, 3);
            }
        }
        let coords = generate(&tiles, &defs());
        let w = 4usize;
        // Row 1, cell 1 (row-major: row * w + col).
        let (col, row) = coords[w + 1];
        assert_eq!((col, row), (0, 2), "interior takes the center variant");
    }

    #[test]
    fn border_tiles_take_a_masked_edge() {
        // Only the bottom row is solid: all three tiles have an empty T neighbor -> x0x-111-x1x matches.
        // After clamping, corner tiles' left/right out-of-bounds neighbors become themselves (EdgesExtend), and the mask still matches --
        // so all three take the top-edge variant (0,0), matching vanilla.
        let mut tiles = Tileset::new("Gameplay", 3, 3);
        for x in 0..3 {
            tiles.set(x, 2, 3); // only the bottom row is solid
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

    /// `GenerateBox` (the `IntroCrusher` box): out-of-bounds = empty, so an inner cell's
    /// 2-away probe leaves the box -> padding; room mode (extends) connects everywhere -> center.
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
        tiles.set(1, 0, 9); // '9' is not defined
        let coords = generate(&tiles, &defs());
        assert_eq!(coords[1], (0, 0), "undefined ids stay at (0,0)");
    }

    #[test]
    fn ignores_change_connectivity() {
        // '3' ignores '*': '3' does not connect to any other id -- but the same id always connects,
        // so a 3x3 all-'3' grid is still fully connected.
        let mut doc = defs();
        doc.tilesets.get_mut("3").unwrap().ignores = vec!['*'];
        let tiles = full_grid();
        let coords = generate(&tiles, &doc);
        let w = 3usize;
        // Row 1, cell 1 (row-major: row * w + col).
        let (col, row) = coords[w + 1];
        assert_eq!((col, row), (0, 2), "same id always connects");
    }

    /// golden: a real map + real tileset definitions must produce **varied** tile coordinates.
    ///
    /// This is the direct regression guard for "everything uses the same texture": if the autotiler degrades
    /// so that every tile takes the same (col,row), this test goes red. The data lives in gitignored
    /// `assets/`, so it is skipped when missing.
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
            "expected varied tile coordinates, got {used:?} - \
             the autotiler collapsed every tile onto one variant"
        );
    }
}
