//! 实体稳定 ID。
//!
//! 规则：`blake3(area_id || room_name || kind_id || x || y)` 取前 8 字节。

use std::fmt;

use serde::{Deserialize, Serialize};

/// 稳定的实体标识符（8 字节）。
///
/// 由地图数据散列生成，确保在热重载之间一致性。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EntityId(u64);

impl EntityId {
    /// 从 `u64` 构造。
    pub const fn new(raw: u64) -> Self {
        EntityId(raw)
    }

    /// 从散列输入数据生成。
    pub fn generate(area_id: u32, room_name: &str, kind_id: u32, x: i32, y: i32) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&area_id.to_le_bytes());
        hasher.update(room_name.as_bytes());
        hasher.update(&kind_id.to_le_bytes());
        hasher.update(&x.to_le_bytes());
        hasher.update(&y.to_le_bytes());
        let hash = hasher.finalize();
        let bytes: [u8; 32] = hash.into();
        let short = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
        EntityId(short)
    }

    /// 原始值。
    pub fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Debug for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EntityId({:016x})", self.0)
    }
}

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_stable() {
        let a = EntityId::generate(1, "room_a", 42, 100, 200);
        let b = EntityId::generate(1, "room_a", 42, 100, 200);
        assert_eq!(a, b);
    }

    #[test]
    fn different_inputs_produce_different_ids() {
        let a = EntityId::generate(1, "room_a", 42, 100, 200);
        let b = EntityId::generate(1, "room_a", 42, 101, 200);
        assert_ne!(a, b);
    }
}
