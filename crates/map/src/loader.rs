//! `.map` 文件加载与保存。

use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::format::{MapDocument, MAP_MAGIC, MAP_VERSION};

/// 带版本头部的磁盘格式。
#[derive(Serialize, Deserialize)]
struct MapDisk {
    magic: [u8; 8],
    version: u32,
    document: MapDocument,
}

/// 加载 `.map` 文件。
pub fn load_map(path: &Path) -> Result<MapDocument, MapError> {
    let bytes = fs::read(path).map_err(MapError::Io)?;
    load_map_bytes(&bytes)
}

/// 从字节加载。
pub fn load_map_bytes(bytes: &[u8]) -> Result<MapDocument, MapError> {
    let disk: MapDisk = bincode::deserialize(bytes).map_err(MapError::Parse)?;

    if disk.magic != *MAP_MAGIC {
        return Err(MapError::InvalidMagic);
    }
    if disk.version != MAP_VERSION {
        return Err(MapError::VersionMismatch {
            found: disk.version,
            expected: MAP_VERSION,
        });
    }

    Ok(disk.document)
}

/// 保存 `.map` 文件。
pub fn save_map(map: &MapDocument, path: &Path) -> Result<(), MapError> {
    let disk = MapDisk {
        magic: *MAP_MAGIC,
        version: MAP_VERSION,
        document: map.clone(),
    };
    let bytes = bincode::serialize(&disk).map_err(MapError::Serialize)?;
    fs::write(path, bytes).map_err(MapError::Io)?;
    Ok(())
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

/// 地图错误。
#[derive(Debug, thiserror::Error)]
pub enum MapError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid magic bytes")]
    InvalidMagic,
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

    #[test]
    fn roundtrip() {
        let map = MapDocument {
            area: "Test".into(),
            rooms: vec![],
            sync_rules: vec![],
        };
        let bytes = to_bytes(&map).unwrap();
        let loaded = load_map_bytes(&bytes).unwrap();
        assert_eq!(loaded.area, "Test");
    }
}
