//! EntityFactory trait、BuildCtx、Persistence。

use crate::entity::{Entity, EntityData};
use crate::schema::Schema;

/// 实体种类标识符。
///
/// 每个注册的实体工厂有一个唯一 `EntityKindId`。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct EntityKindId(pub u32);

impl EntityKindId {
    pub const fn new(raw: u32) -> Self {
        EntityKindId(raw)
    }
}

/// 实体工厂：构建运行时 [`Entity`] 并声明其 Schema 与持久化策略。
pub trait EntityFactory: Send + Sync {
    /// 本工厂对应的实体种类标识。
    fn id(&self) -> EntityKindId;

    /// 从地图数据构建运行时实体。
    fn build(&self, data: &EntityData, ctx: &mut BuildCtx) -> Entity;

    /// 返回属性 Schema（编辑器用来自动生成面板）。
    fn schema(&self) -> &Schema;

    /// 热重载时的持久化策略。
    fn persistence(&self) -> Persistence;
}

/// 实体在热重载时的持久化策略。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Persistence {
    /// 热重载时重建（墙壁、装饰等静态实体）。
    Rebuild,
    /// 保留状态，仅更新属性（可拾取物、门等状态实体）。
    Preserve,
    /// 永不卸载（玩家、摄像机）。
    Persistent,
}

/// 构建上下文。
///
/// 在 `EntityFactory::build` 期间提供对其它实体的查询
/// 及事件发送。
pub struct BuildCtx {
    // 稍后扩展：事件总线、World 读引用等。
}

impl BuildCtx {
    pub fn new() -> Self {
        BuildCtx {}
    }
}

impl Default for BuildCtx {
    fn default() -> Self {
        Self::new()
    }
}
