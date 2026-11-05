//! `.map` file loading and saving.
//!
//! The shape of the read path (ROADMAP Appendix E.7):
//!
//! ```text
//! Header sniff (magic + version, 12 bytes, fixed length)
//!   |-- version >  MAP_VERSION -> FutureVersion (reject loudly, no partial read)
//!   |-- version == MAP_VERSION -> decode the current schema directly
//!   `-- version <  MAP_VERSION -> hand to crate::legacy::migrate for chained migration
//! ```
//!
//! The header is fixed length: `MAGIC` (8 bytes) + `version` (u32 little-endian, 4 bytes). Because
//! bincode encodes both `[u8; 8]` and `u32` at fixed length, **sniff first, then decode** is
//! safe -- which is exactly why we can give a clear error for "a file from a newer engine" instead of a
//! vague `Parse` error (E.9 #4).

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::format::{MapDocument, MAP_MAGIC, MAP_VERSION};

/// Header length: magic (8) + version (4).
const HEADER_LEN: usize = 12;

/// On-disk format with the version header (current version).
#[derive(Serialize, Deserialize)]
struct MapDisk {
    magic: [u8; 8],
    version: u32,
    document: MapDocument,
}

/// Sniff the version number from bytes (without a full decode).
///
/// `None` means even the header is incomplete.
fn peek_version(bytes: &[u8]) -> Option<u32> {
    let header = bytes.get(..HEADER_LEN)?;
    let version = header.get(8..12)?;
    Some(u32::from_le_bytes(version.try_into().ok()?))
}

/// Load a `.map` file.
pub fn load_map(path: &Path) -> Result<MapDocument, MapError> {
    let bytes = fs::read(path).map_err(MapError::Io)?;
    load_map_bytes(&bytes)
}

/// Load from bytes.
///
/// Old-version files are **migrated** to the current schema rather than rejected (AGENTS.md 7.3).
pub fn load_map_bytes(bytes: &[u8]) -> Result<MapDocument, MapError> {
    let Some(version) = peek_version(bytes) else {
        return Err(MapError::InvalidMagic);
    };

    match version {
        v if v > MAP_VERSION => Err(MapError::FutureVersion {
            found: v,
            supported: MAP_VERSION,
        }),
        MAP_VERSION => {
            let disk: MapDisk = bincode::deserialize(bytes).map_err(MapError::Parse)?;
            if disk.magic != *MAP_MAGIC {
                return Err(MapError::InvalidMagic);
            }
            Ok(disk.document)
        }
        _ => {
            // Validate magic first, then hand the confirmed-complete bytes to the migration layer.
            if bytes.first_chunk::<8>() != Some(MAP_MAGIC) {
                return Err(MapError::InvalidMagic);
            }
            crate::legacy::migrate(version, bytes)
        }
    }
}

/// Save a `.map` file.
///
/// **Atomic write**: first write `<name>.tmp` in the same directory, `fsync`, then `rename` over
/// the target (ROADMAP E.8). `rename` is atomic on POSIX and on same-volume NTFS,
/// so `FileWatcher` can only ever see a complete file -- otherwise a crash halfway through a write would feed
/// a truncated file to the parser (E.9 #6).
pub fn save_map(map: &MapDocument, path: &Path) -> Result<(), MapError> {
    let bytes = to_bytes(map)?;
    write_atomically(path, &bytes)
}

/// Serialize to bytes.
pub fn to_bytes(map: &MapDocument) -> Result<Vec<u8>, MapError> {
    let disk = MapDisk {
        magic: *MAP_MAGIC,
        version: MAP_VERSION,
        document: map.clone(),
    };
    bincode::serialize(&disk).map_err(MapError::Serialize)
}

/// Atomic write: temp file -> fsync -> rename.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), MapError> {
    let tmp = tmp_path(path);

    // Teardown goes through a single cleanup path: no matter which of write/sync/rename fails, the temp
    // file is not left behind (AGENTS.md 15.9: errors must not be swallowed, but garbage must not be left either).
    let result = (|| -> io::Result<()> {
        let mut file = File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)
    })();

    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(MapError::Io)
}

/// Temp file path: same directory, with a `.tmp` suffix.
///
/// It must be the same directory: a cross-filesystem `rename` degrades to copy + delete and is no longer atomic.
fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Map error.
#[derive(Debug, thiserror::Error)]
pub enum MapError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid magic bytes")]
    InvalidMagic,
    /// The file comes from a **newer** engine.
    ///
    /// A separate variant: such a file is not "broken", and it should not be treated as a parse failure --
    /// we simply cannot read it, and we can very likely read half of it (ROADMAP E.7).
    #[error("map version {found} is newer than supported version {supported}")]
    FutureVersion { found: u32, supported: u32 },
    /// The file comes from an **older version we no longer support** (there is no such link in the migration chain).
    #[error("map version {found} is not supported")]
    UnsupportedVersion { found: u32 },
    #[error("version mismatch: found {found}, expected {expected}")]
    VersionMismatch { found: u32, expected: u32 },
    #[error("parse error: {0}")]
    Parse(#[source] bincode::Error),
    #[error("serialize error: {0}")]
    Serialize(#[source] bincode::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{MapStyle, SyncRule, Tileset};
    use crate::legacy::fixtures;

    fn sample() -> MapDocument {
        MapDocument {
            area: "Test".into(),
            rooms: vec![crate::format::Room {
                name: "lvl_0".into(),
                bounds: crate::format::RectDef {
                    x: 0.0,
                    y: 0.0,
                    w: 320.0,
                    h: 184.0,
                },
                tiles: Tileset::new("Gameplay", 40, 23),
                bg: Tileset::new("BackgroundTiles", 40, 23),
                entities: vec![],
                triggers: vec![],
                script: None,
                bg_decals: vec![],
                fg_decals: vec![],
                bg_scenery: Tileset::default(),
                fg_scenery: Tileset::default(),
            }],
            sync_rules: vec![SyncRule {
                kind: "door".into(),
                mode: crate::format::SyncMode::Flag,
                flag: Some("open".into()),
            }],
            style: MapStyle::default(),
        }
    }

    #[test]
    fn roundtrip() {
        let map = sample();
        let bytes = to_bytes(&map).unwrap();
        let loaded = load_map_bytes(&bytes).unwrap();
        assert_eq!(loaded, map);
    }

    /// E.11: byte-level roundtrip, not just structural equality.
    #[test]
    fn roundtrip_is_byte_exact() {
        let map = sample();
        let bytes = to_bytes(&map).unwrap();
        let loaded = load_map_bytes(&bytes).unwrap();
        assert_eq!(to_bytes(&loaded).unwrap(), bytes);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = to_bytes(&sample()).unwrap();
        bytes[0] = b'X';
        assert!(matches!(
            load_map_bytes(&bytes),
            Err(MapError::InvalidMagic)
        ));
    }

    #[test]
    fn rejects_truncated_header() {
        assert!(matches!(
            load_map_bytes(b"RELMAP"),
            Err(MapError::InvalidMagic)
        ));
        assert!(matches!(load_map_bytes(b""), Err(MapError::InvalidMagic)));
    }

    /// E.11: a newer file -> `FutureVersion`, not a parse error.
    #[test]
    fn future_version_is_named_not_a_parse_error() {
        let bytes = fixtures::to_bytes_with_version(&sample(), 99);
        match load_map_bytes(&bytes) {
            Err(MapError::FutureVersion { found, supported }) => {
                assert_eq!(found, 99);
                assert_eq!(supported, MAP_VERSION);
            }
            other => panic!("expected FutureVersion, got {other:?}"),
        }
    }

    /// E.11: the v1 fixture is still readable, and no v1 field is missing.
    #[test]
    fn v1_fixture_still_loads_and_migrates() {
        let map = sample();
        let bytes = fixtures::to_v1_bytes(&map);
        let loaded = load_map_bytes(&bytes).expect("v1 must migrate");
        fixtures::assert_v1_fields_survive(&map, &loaded).unwrap();
        assert_eq!(loaded.style, MapStyle::default());
    }

    /// E.11 (golden): after the v2 backdrop migrates to v3, existing fields are byte-identical and
    /// the added room filtering takes "unset", the transition visibility takes the vanilla default.
    #[test]
    fn v2_fixture_still_loads_and_migrates() {
        let mut map = sample();
        let mut bd = crate::format::Backdrop::new("parallax");
        bd.texture = "bgs/00/bg0".into();
        bd.x = -24.0;
        bd.y = -64.0;
        bd.scroll_x = 0.15;
        bd.scroll_y = 0.05;
        bd.loop_x = true;
        bd.loop_y = false;
        bd.color = [140, 140, 140, 140];
        map.style.backgrounds.push(bd);

        let bytes = fixtures::to_v2_bytes(&map);
        let loaded = load_map_bytes(&bytes).expect("v2 must migrate");
        assert_eq!(loaded.area, map.area);
        assert_eq!(loaded.rooms, map.rooms);
        assert_eq!(loaded.sync_rules, map.sync_rules);
        let got = &loaded.style.backgrounds[0];
        let want = &map.style.backgrounds[0];
        assert_eq!(got.kind, want.kind);
        assert_eq!(got.texture, want.texture);
        assert_eq!(got.atlas, want.atlas);
        assert_eq!((got.x, got.y), (want.x, want.y));
        assert_eq!((got.scroll_x, got.scroll_y), (want.scroll_x, want.scroll_y));
        assert_eq!((got.loop_x, got.loop_y), (want.loop_x, want.loop_y));
        assert_eq!(got.color, want.color);
        // Fields added in v3: a v2 file has no expressiveness -> filtering empty, visibility at the vanilla default
        // (InstantIn = true, InstantOut = false, pureCeleste.cs:109411-109418).
        assert!(got.only_in.is_empty());
        assert!(got.exclude_from.is_empty());
        assert!(got.instant_in);
        assert!(!got.instant_out);
    }

    /// v0 has no migration chain, so it must be named explicitly rather than treated as a parse failure.
    #[test]
    fn v0_is_unsupported_not_a_parse_error() {
        let bytes = fixtures::to_bytes_with_version(&sample(), 0);
        assert!(matches!(
            load_map_bytes(&bytes),
            Err(MapError::UnsupportedVersion { found: 0 })
        ));
    }

    /// E.8: the same serde serialization must be byte-stable (the point of `BTreeMap`).
    #[test]
    fn serialisation_is_deterministic() {
        let map = sample();
        assert_eq!(to_bytes(&map).unwrap(), to_bytes(&map).unwrap());
    }

    #[test]
    fn save_is_atomic_and_leaves_no_temp_file() {
        let dir = std::env::temp_dir().join("releste-map-atomic");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.map");

        save_map(&sample(), &path).unwrap();
        assert!(path.is_file());
        assert!(
            !tmp_path(&path).exists(),
            "temp file must be renamed away, not left behind"
        );

        // Overwriting an existing file likewise leaves no residue.
        let mut other = sample();
        other.area = "Test2".into();
        save_map(&other, &path).unwrap();
        assert_eq!(load_map(&path).unwrap().area, "Test2");
        assert!(!tmp_path(&path).exists());

        let _ = fs::remove_dir_all(&dir);
    }
}
