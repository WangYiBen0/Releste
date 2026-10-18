//! `.map` 文件加载与保存。
//!
//! 读取路径的形态（ROADMAP 附录 E.7）：
//!
//! ```text
//! 头部嗅探（magic + version，12 字节，定长）
//!   ├── version >  MAP_VERSION → FutureVersion（响亮地拒绝，不半读）
//!   ├── version == MAP_VERSION → 直接解码当前 schema
//!   └── version <  MAP_VERSION → 交给 crate::legacy::migrate 链式迁移
//! ```
//!
//! 头部是定长的：`MAGIC`（8 字节）+ `version`（u32 小端 4 字节）。因为
//! bincode 对 `[u8; 8]` 和 `u32` 都是定长编码，所以**先嗅探再解码**是
//! 安全的 —— 这也正是能对"来自更新引擎的文件"给出明确错误而不是一个
//! 语义不明的 `Parse` 错误的原因（E.9 #4）。

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::format::{MapDocument, MAP_MAGIC, MAP_VERSION};

/// 头部长度：magic（8） + version（4）。
const HEADER_LEN: usize = 12;

/// 带版本头部的磁盘格式（当前版本）。
#[derive(Serialize, Deserialize)]
struct MapDisk {
    magic: [u8; 8],
    version: u32,
    document: MapDocument,
}

/// 从字节里嗅探版本号（不做完整解码）。
///
/// `None` 表示连头部都不完整。
fn peek_version(bytes: &[u8]) -> Option<u32> {
    let header = bytes.get(..HEADER_LEN)?;
    let version = header.get(8..12)?;
    Some(u32::from_le_bytes(version.try_into().ok()?))
}

/// 加载 `.map` 文件。
pub fn load_map(path: &Path) -> Result<MapDocument, MapError> {
    let bytes = fs::read(path).map_err(MapError::Io)?;
    load_map_bytes(&bytes)
}

/// 从字节加载。
///
/// 旧版本文件会被**迁移**到当前 schema，而不是拒绝（AGENTS.md §7.3）。
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
            // 先校验 magic，再让迁移层拿到已确认完整的字节。
            if bytes.first_chunk::<8>() != Some(MAP_MAGIC) {
                return Err(MapError::InvalidMagic);
            }
            crate::legacy::migrate(version, bytes)
        }
    }
}

/// 保存 `.map` 文件。
///
/// **原子写**：先写同目录的 `<name>.tmp`，`fsync`，再 `rename` 覆盖
/// （ROADMAP E.8）。`rename` 在 POSIX 和同卷 NTFS 上都是原子的，
/// 所以 `FileWatcher` 只可能看到完整文件 —— 否则崩在写一半会把截断的
/// 文件喂给解析器（E.9 #6）。
pub fn save_map(map: &MapDocument, path: &Path) -> Result<(), MapError> {
    let bytes = to_bytes(map)?;
    write_atomically(path, &bytes)
}

/// 序列化到字节。
pub fn to_bytes(map: &MapDocument) -> Result<Vec<u8>, MapError> {
    let disk = MapDisk {
        magic: *MAP_MAGIC,
        version: MAP_VERSION,
        document: map.clone(),
    };
    bincode::serialize(&disk).map_err(MapError::Serialize)
}

/// 原子写：临时文件 → fsync → rename。
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), MapError> {
    let tmp = tmp_path(path);

    // 收尾统一走同一个清理路径：无论写/同步/改名在哪一步失败，临时
    // 文件都不留在地上（AGENTS.md §15.9：错误不能吞，但也不能留垃圾）。
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

/// 临时文件路径：同目录，加 `.tmp` 后缀。
///
/// 必须同目录：跨文件系统的 `rename` 会退化成复制 + 删除，不再原子。
fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".tmp");
    path.with_file_name(name)
}

/// 地图错误。
#[derive(Debug, thiserror::Error)]
pub enum MapError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid magic bytes")]
    InvalidMagic,
    /// 文件来自**更新**的引擎。
    ///
    /// 单独一个变体：这种文件不是"坏了"，也不该被当作解析失败 ——
    /// 它只是我们读不懂，而且很可能读得懂一半（ROADMAP E.7）。
    #[error("map version {found} is newer than supported version {supported}")]
    FutureVersion { found: u32, supported: u32 },
    /// 文件来自一个**我们不再支持**的更老版本（迁移链里没这一环）。
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

    /// E.11：字节级 roundtrip，不只是结构相等。
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

    /// E.11：更新的文件 -> `FutureVersion`，而不是解析错误。
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

    /// E.11：v1 夹具仍然能读，且 v1 已有的字段一个不少。
    #[test]
    fn v1_fixture_still_loads_and_migrates() {
        let map = sample();
        let bytes = fixtures::to_v1_bytes(&map);
        let loaded = load_map_bytes(&bytes).expect("v1 must migrate");
        fixtures::assert_v1_fields_survive(&map, &loaded).unwrap();
        assert_eq!(loaded.style, MapStyle::default());
    }

    /// E.11（金样）：v2 的 backdrop 迁到 v3 后，既有字段逐字节一致，
    /// 新增的房间过滤取"未设置"、转场可见性取原版默认。
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
        // v3 新增字段：v2 文件没有表达力 → 过滤为空、可见性取原版默认
        // （InstantIn = true，InstantOut = false，pureCeleste.cs:109411-109418）。
        assert!(got.only_in.is_empty());
        assert!(got.exclude_from.is_empty());
        assert!(got.instant_in);
        assert!(!got.instant_out);
    }

    /// v0 没有迁移链，必须明确点名而不是当成解析失败。
    #[test]
    fn v0_is_unsupported_not_a_parse_error() {
        let bytes = fixtures::to_bytes_with_version(&sample(), 0);
        assert!(matches!(
            load_map_bytes(&bytes),
            Err(MapError::UnsupportedVersion { found: 0 })
        ));
    }

    /// E.8：同一次 serde 直列化必须字节稳定（`BTreeMap` 的意义）。
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

        // 覆盖已有文件同样不留残渣。
        let mut other = sample();
        other.area = "Test2".into();
        save_map(&other, &path).unwrap();
        assert_eq!(load_map(&path).unwrap().area, "Test2");
        assert!(!tmp_path(&path).exists());

        let _ = fs::remove_dir_all(&dir);
    }
}
