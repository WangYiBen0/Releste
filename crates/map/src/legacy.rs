//! 历史版本 schema 的**冻结**副本。
//!
//! `bincode` 不自描述（ROADMAP 附录 E.6）：动一个字段等于让所有旧文件
//! 解码失败。因此"读旧文件"必须靠**旧结构体 + 迁移函数**，而不是靠
//! `#[serde(default)]`（在 bincode 下永远不生效，E.9 #5）。
//!
//! 这里的结构体一旦发布就**不得再改**：它们的字节布局就是磁盘上的
//! 事实。任何演进都在 [`crate::format`] 里做，并在这里留下新的一份。
//!
//! 当前持有：
//! - [`v1`] —— 引入 `nodes` / `style` / 贴花 / 装饰层之前的版本。

use serde::{Deserialize, Serialize};

use releste_world::{EntityId, FieldValue};

/// `MAP_VERSION == 1` 时的完整磁盘结构（只用于解码旧文件）。
///
/// 字段顺序与 `crates/map/src/format.rs` 在 v1 时的声明顺序**逐一对应**，
/// 顺序即磁盘布局，不得调整。
///
/// 注意 [`v1::EntityData`] 是 v1 自己的副本，**不是**
/// `releste_world::EntityData`：后者在 v2 追加了 `nodes`，直接复用会让
/// 旧文件从 `nodes` 处开始读错位。这正是"冻结副本"存在的理由。
///
/// 唯一刻意的偏离：`properties` 用 `BTreeMap` 而不是 v1 当时写的
/// `HashMap`。bincode 对两者的编码**逐字节相同**（长度前缀 + 键值对
/// 序列），所以磁盘布局不变；换来的是不再引入 `HashMap` 的随机顺序。
pub mod v1 {
    use std::collections::BTreeMap;

    use super::*;

    /// v1 的实体数据（`releste_world::EntityData` 去掉 `nodes`）。
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct EntityData {
        pub id: EntityId,
        pub kind: String,
        pub x: f32,
        pub y: f32,
        pub width: Option<i32>,
        pub height: Option<i32>,
        pub origin_x: f32,
        pub origin_y: f32,
        pub properties: BTreeMap<String, FieldValue>,
    }

    /// 带版本头部的磁盘格式。
    #[derive(Debug, Serialize, Deserialize)]
    pub struct MapDisk {
        pub magic: [u8; 8],
        pub version: u32,
        pub document: MapDocument,
    }

    /// v1 的地图文档。
    #[derive(Debug, Serialize, Deserialize)]
    pub struct MapDocument {
        pub area: String,
        pub rooms: Vec<Room>,
        #[serde(default)]
        pub sync_rules: Vec<SyncRule>,
    }

    /// v1 的房间。
    #[derive(Debug, Serialize, Deserialize)]
    pub struct Room {
        pub name: String,
        pub bounds: RectDef,
        pub tiles: Tileset,
        #[serde(default)]
        pub bg: Tileset,
        #[serde(default)]
        pub entities: Vec<EntityData>,
        #[serde(default)]
        pub triggers: Vec<TriggerData>,
        #[serde(default)]
        pub script: Option<std::path::PathBuf>,
    }

    /// v1 的矩形定义。
    #[derive(Debug, Serialize, Deserialize)]
    pub struct RectDef {
        pub x: f32,
        pub y: f32,
        pub w: f32,
        pub h: f32,
    }

    /// v1 的 tile 网格。
    #[derive(Debug, Default, Serialize, Deserialize)]
    pub struct Tileset {
        #[serde(default)]
        pub atlas: String,
        #[serde(default)]
        pub width: u32,
        #[serde(default)]
        pub height: u32,
        #[serde(default)]
        pub tiles: Vec<u8>,
    }

    /// v1 的触发器。
    #[derive(Debug, Serialize, Deserialize)]
    pub struct TriggerData {
        pub kind: String,
        pub x: f32,
        pub y: f32,
        pub width: f32,
        pub height: f32,
        #[serde(default)]
        pub properties: BTreeMap<String, String>,
    }

    /// v1 的同步规则。
    #[derive(Debug, Serialize, Deserialize)]
    pub struct SyncRule {
        pub kind: String,
        pub mode: SyncMode,
        #[serde(default)]
        pub flag: Option<String>,
    }

    /// v1 的同步模式。
    #[derive(Debug, Serialize, Deserialize)]
    pub enum SyncMode {
        Flag,
        Full,
        Event,
    }
}

impl From<v1::SyncMode> for crate::format::SyncMode {
    fn from(value: v1::SyncMode) -> Self {
        match value {
            v1::SyncMode::Flag => crate::format::SyncMode::Flag,
            v1::SyncMode::Full => crate::format::SyncMode::Full,
            v1::SyncMode::Event => crate::format::SyncMode::Event,
        }
    }
}

impl From<crate::format::SyncMode> for v1::SyncMode {
    fn from(value: crate::format::SyncMode) -> Self {
        match value {
            crate::format::SyncMode::Flag => v1::SyncMode::Flag,
            crate::format::SyncMode::Full => v1::SyncMode::Full,
            crate::format::SyncMode::Event => v1::SyncMode::Event,
        }
    }
}

impl From<v1::Tileset> for crate::format::Tileset {
    fn from(value: v1::Tileset) -> Self {
        crate::format::Tileset {
            atlas: value.atlas,
            width: value.width,
            height: value.height,
            // v1 是 `Vec<u8>`，v2 放宽到 `Vec<u16>`（装饰层下标能到 352）。
            // 逐元素加宽：`u8` → `u16` 永远不丢信息。
            tiles: value.tiles.into_iter().map(u16::from).collect(),
        }
    }
}

impl From<v1::EntityData> for releste_world::EntityData {
    fn from(value: v1::EntityData) -> Self {
        // `nodes` 是 v2 才有的表达力：旧文件里不存在，迁移后取空。
        releste_world::EntityData {
            id: value.id,
            kind: value.kind,
            x: value.x,
            y: value.y,
            width: value.width,
            height: value.height,
            origin_x: value.origin_x,
            origin_y: value.origin_y,
            properties: value.properties,
            nodes: Vec::new(),
        }
    }
}

/// 把一个 v1 文档迁移到当前版本。
///
/// 迁移是**纯函数**：只做数据搬运，不碰磁盘、不碰全局状态，因此可以
/// 串联（`v0 → v1 → v2 …`，ROADMAP E.7）。
///
/// v2 新增的字段一律取"旧文件没有表达力"的那个值 —— 空。旧版本根本
/// 没有视差背景、贴花和装饰层，所以迁移后的文档渲染出来与旧解释器
/// **逐像素一致**：不会凭空多出东西，也不会少。
///
/// 出处：`pureCeleste.cs:112593-112618`（`CreateBackdrops`、
/// `112428-112449` 的 `<Style>`）、`77244-77253`（`TileGrid.Overlay`）、
/// `113467-113497`（`bgdecals` / `fgdecals` 解析）、
/// `113135-113198`（`EntityData.Nodes`）。
pub fn migrate_v1_to_v2(doc: v1::MapDocument) -> crate::format::MapDocument {
    crate::format::MapDocument {
        area: doc.area,
        rooms: doc
            .rooms
            .into_iter()
            .map(|room| crate::format::Room {
                name: room.name,
                bounds: crate::format::RectDef {
                    x: room.bounds.x,
                    y: room.bounds.y,
                    w: room.bounds.w,
                    h: room.bounds.h,
                },
                tiles: room.tiles.into(),
                bg: room.bg.into(),
                entities: room.entities.into_iter().map(Into::into).collect(),
                triggers: room
                    .triggers
                    .into_iter()
                    .map(|t| crate::format::TriggerData {
                        kind: t.kind,
                        x: t.x,
                        y: t.y,
                        width: t.width,
                        height: t.height,
                        properties: t.properties,
                    })
                    .collect(),
                script: room.script,
                bg_decals: Vec::new(),
                fg_decals: Vec::new(),
                bg_scenery: crate::format::Tileset::default(),
                fg_scenery: crate::format::Tileset::default(),
            })
            .collect(),
        sync_rules: doc
            .sync_rules
            .into_iter()
            .map(|r| crate::format::SyncRule {
                kind: r.kind,
                mode: r.mode.into(),
                flag: r.flag,
            })
            .collect(),
        style: crate::format::MapStyle::default(),
    }
}

/// 把任意版本的文档迁移到当前版本。
///
/// 链式：每加一版就多一个 `v_n → v_{n+1}` 分支，N 版之前的老文件
/// 依然能不加特例地读进来（ROADMAP E.7）。
pub fn migrate(version: u32, bytes: &[u8]) -> Result<crate::format::MapDocument, crate::MapError> {
    match version {
        0 => Err(crate::MapError::UnsupportedVersion { found: version }),
        1 => {
            let disk: v1::MapDisk = bincode::deserialize(bytes).map_err(crate::MapError::Parse)?;
            if disk.magic != *crate::format::MAP_MAGIC {
                return Err(crate::MapError::InvalidMagic);
            }
            Ok(migrate_v1_to_v2(disk.document))
        }
        v => Err(crate::MapError::UnsupportedVersion { found: v }),
    }
}

/// 夹具与测试用的构造辅助。
///
/// 保留在非 `cfg(test)` 下：`tests/` 集成测试和夹具生成器都要用它写出
/// "真正的旧文件"。夹具由当前 schema **降级**得到，所以不会腐烂。
pub mod fixtures {
    use super::*;
    use crate::format::MapDocument as Current;

    /// v2 的 `u16` tile 下标 → v1 的 `u8`，只在放得下时保留。
    ///
    /// 这是**测试 fixture 专用**的降级方向：真实迁移只会 v1 → v2。
    fn narrowed(tiles: &[u16]) -> Vec<u8> {
        tiles
            .iter()
            .map(|t| u8::try_from(*t).unwrap_or(0))
            .collect()
    }

    /// 从当前文档里丢掉 v2 新增的东西，得到它的 v1 形态。
    pub fn downgrade(doc: &Current) -> v1::MapDocument {
        v1::MapDocument {
            area: doc.area.clone(),
            rooms: doc
                .rooms
                .iter()
                .map(|r| v1::Room {
                    name: r.name.clone(),
                    bounds: v1::RectDef {
                        x: r.bounds.x,
                        y: r.bounds.y,
                        w: r.bounds.w,
                        h: r.bounds.h,
                    },
                    tiles: v1::Tileset {
                        atlas: r.tiles.atlas.clone(),
                        width: r.tiles.width,
                        height: r.tiles.height,
                        // 反向收窄：v2 → v1 只在全部下标 ≤ 255 时无损。
                        // fixture 用的是自己造的窄网格，不会踩到。
                        tiles: narrowed(&r.tiles.tiles),
                    },
                    bg: v1::Tileset {
                        atlas: r.bg.atlas.clone(),
                        width: r.bg.width,
                        height: r.bg.height,
                        tiles: narrowed(&r.bg.tiles),
                    },
                    entities: r
                        .entities
                        .iter()
                        .map(|e| v1::EntityData {
                            id: e.id,
                            kind: e.kind.clone(),
                            x: e.x,
                            y: e.y,
                            width: e.width,
                            height: e.height,
                            origin_x: e.origin_x,
                            origin_y: e.origin_y,
                            properties: e.properties.clone(),
                        })
                        .collect(),
                    triggers: r
                        .triggers
                        .iter()
                        .map(|t| v1::TriggerData {
                            kind: t.kind.clone(),
                            x: t.x,
                            y: t.y,
                            width: t.width,
                            height: t.height,
                            properties: t.properties.clone(),
                        })
                        .collect(),
                    script: r.script.clone(),
                })
                .collect(),
            sync_rules: doc
                .sync_rules
                .iter()
                .map(|r| v1::SyncRule {
                    kind: r.kind.clone(),
                    mode: r.mode.clone().into(),
                    flag: r.flag.clone(),
                })
                .collect(),
        }
    }

    /// 把 v1 文档写成磁盘字节（含版本头）。
    pub fn to_v1_bytes(doc: &Current) -> Vec<u8> {
        let disk = v1::MapDisk {
            magic: *crate::format::MAP_MAGIC,
            version: 1,
            document: downgrade(doc),
        };
        bincode::serialize(&disk).expect("v1 fixture must serialize")
    }

    /// 把文档写成任意版本号的字节（用于"未来版本"夹具）。
    pub fn to_bytes_with_version(doc: &Current, version: u32) -> Vec<u8> {
        let disk = v1::MapDisk {
            magic: *crate::format::MAP_MAGIC,
            version,
            document: downgrade(doc),
        };
        bincode::serialize(&disk).expect("fixture must serialize")
    }

    /// 迁移后**必须**逐字段相等的那部分（v2 新增字段除外）。
    ///
    /// 用于迁移测试：当前文档 → 降级成 v1 字节 → 迁移回 v2，v1 本来就
    /// 有的字段一个都不能变。`nodes`（v2 新增）不参与比较。
    pub fn assert_v1_fields_survive(before: &Current, after: &Current) -> Result<(), String> {
        if before.area != after.area {
            return Err(format!("area: {:?} != {:?}", before.area, after.area));
        }
        if before.rooms.len() != after.rooms.len() {
            return Err(format!(
                "room count: {} != {}",
                before.rooms.len(),
                after.rooms.len()
            ));
        }
        for (a, b) in before.rooms.iter().zip(after.rooms.iter()) {
            if a.name != b.name {
                return Err(format!("room name: {} != {}", a.name, b.name));
            }
            if a.bounds != b.bounds {
                return Err(format!("room {} bounds differ", a.name));
            }
            if a.tiles != b.tiles {
                return Err(format!("room {} tiles differ", a.name));
            }
            if a.bg != b.bg {
                return Err(format!("room {} bg differ", a.name));
            }
            if a.triggers != b.triggers {
                return Err(format!("room {} triggers differ", a.name));
            }
            if a.script != b.script {
                return Err(format!("room {} script differ", a.name));
            }
            if a.entities.len() != b.entities.len() {
                return Err(format!("room {} entity count differs", a.name));
            }
            for (ea, eb) in a.entities.iter().zip(b.entities.iter()) {
                if ea.id != eb.id
                    || ea.kind != eb.kind
                    || ea.x != eb.x
                    || ea.y != eb.y
                    || ea.width != eb.width
                    || ea.height != eb.height
                    || ea.origin_x != eb.origin_x
                    || ea.origin_y != eb.origin_y
                    || ea.properties != eb.properties
                {
                    return Err(format!("entity {} in room {} differs", ea.id, a.name));
                }
            }
        }
        if before.sync_rules != after.sync_rules {
            return Err("sync_rules differ after migration".to_string());
        }
        Ok(())
    }
}
