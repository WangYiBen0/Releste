//! Service trait、ServiceRegistry、Context。

use std::any::{Any, TypeId};
use std::collections::HashMap;

use slotmap::{DefaultKey, SlotMap};

use crate::event::EventBus;
use crate::frame::FrameId;
use reles_math::Fx;

/// Service 的唯一标识。
///
/// 由 service 自己声明（如 `ServiceId::new("input")`），
/// 便于日志与调试。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ServiceId(&'static str);

impl ServiceId {
    /// 构造。
    pub const fn new(name: &'static str) -> Self {
        ServiceId(name)
    }

    /// 名称。
    pub const fn name(self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for ServiceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// 引擎服务。
///
/// 所有系统（仿真、渲染、音频……）都实现此 trait。
pub trait Service: 'static {
    /// 服务 ID（自己声明，全局唯一）。
    fn id(&self) -> ServiceId;

    /// 初始化：在这里注册事件订阅、获取其它 service 引用。
    fn attach(&mut self, ctx: &mut Context);

    /// 每帧更新。`dt` 恒为 [`FIXED_DT`](reles_math::FIXED_DT)。
    fn update(&mut self, ctx: &mut Context, frame: FrameId, dt: Fx);

    /// 释放资源。
    fn dispose(&mut self) {}
}

/// Service 运行上下文。
///
/// 提供事件总线访问。Service 之间的依赖在 `attach` 阶段
/// 由外部显式注入。
pub struct Context<'a> {
    pub(crate) bus: &'a mut EventBus,
}

impl<'a> Context<'a> {
    /// 构造上下文。
    ///
    /// 主要供测试与嵌入式宿主使用；正常主循环由 [`Kernel`](crate::Kernel)
    /// 创建。
    pub fn new(bus: &'a mut EventBus) -> Self {
        Context { bus }
    }

    /// 广播事件。
    pub fn send<T: Any + Send>(&self, event: T) {
        self.bus.send(event);
    }

    /// 注册事件订阅。
    ///
    /// 每种事件类型只能有一个订阅者；重复订阅返回
    /// [`SubscribeError::AlreadySubscribed`](crate::event::SubscribeError::AlreadySubscribed)。
    pub fn subscribe<T: Any + Send>(
        &mut self,
    ) -> Result<flume::Receiver<T>, crate::event::SubscribeError> {
        self.bus.with_subscriber()
    }
}

/// Service 注册表。
///
/// 保存所有 service，并支持按类型与按 [`ServiceId`] 查找。
pub struct ServiceRegistry {
    services: SlotMap<DefaultKey, Box<dyn AnyService>>,
    by_type: HashMap<TypeId, DefaultKey>,
    by_name: HashMap<ServiceId, DefaultKey>,
}

/// 类型擦除的 service。
trait AnyService: 'static {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn as_service(&mut self) -> &mut dyn Service;
}

impl<S: Service> AnyService for S {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn as_service(&mut self) -> &mut dyn Service {
        self
    }
}

impl ServiceRegistry {
    /// 创建空注册表。
    pub fn new() -> Self {
        ServiceRegistry {
            services: SlotMap::with_key(),
            by_type: HashMap::new(),
            by_name: HashMap::new(),
        }
    }

    /// 注册 service，返回其 [`ServiceId`]。
    ///
    /// 若同名 service 已注册，返回 `Err(existing_id)`。
    pub fn register<S: Service>(&mut self, svc: S) -> Result<ServiceId, ServiceId> {
        let id = svc.id();
        if self.by_name.contains_key(&id) {
            return Err(id);
        }
        let key = self.services.insert(Box::new(svc));
        self.by_type.insert(TypeId::of::<S>(), key);
        self.by_name.insert(id, key);
        Ok(id)
    }

    /// 获取 service 引用。
    pub fn get<S: Service>(&self) -> Option<&S> {
        self.by_type
            .get(&TypeId::of::<S>())
            .and_then(|k| self.services.get(*k))
            .and_then(|s| s.as_any().downcast_ref::<S>())
    }

    /// 获取 service 可变引用。
    pub fn get_mut<S: Service>(&mut self) -> Option<&mut S> {
        self.by_type
            .get(&TypeId::of::<S>())
            .and_then(|k| self.services.get_mut(*k))
            .and_then(|s| s.as_any_mut().downcast_mut::<S>())
    }

    /// 按 [`ServiceId`] 获取可变引用（类型擦除）。
    pub fn get_by_id_mut(&mut self, id: ServiceId) -> Option<&mut dyn Service> {
        self.by_name
            .get(&id)
            .and_then(|k| self.services.get_mut(*k))
            .map(|s| s.as_service())
    }

    /// 是否已注册。
    pub fn contains(&self, id: ServiceId) -> bool {
        self.by_name.contains_key(&id)
    }

    /// 所有已注册的 service ID。
    pub fn ids(&self) -> Vec<ServiceId> {
        self.by_name.keys().copied().collect()
    }

    /// 每帧：逐个更新所有 service。
    ///
    /// 先收集 key 再逐个 `get_mut`，避免持有注册表的不可变
    /// 引用时进行可变迭代的借用冲突。
    pub fn update_each(&mut self, bus: &mut EventBus, frame: FrameId, dt: Fx) {
        let keys: Vec<DefaultKey> = self.services.keys().collect();
        for key in keys {
            if let Some(svc) = self.services.get_mut(key) {
                let svc = svc.as_service();
                let mut ctx = Context { bus };
                svc.update(&mut ctx, frame, dt);
            }
        }
    }

    /// Boot 阶段：逐个 `attach` 所有 service。
    pub fn attach_each(&mut self, bus: &mut EventBus) {
        let keys: Vec<DefaultKey> = self.services.keys().collect();
        for key in keys {
            if let Some(svc) = self.services.get_mut(key) {
                let svc = svc.as_service();
                let mut ctx = Context { bus };
                svc.attach(&mut ctx);
            }
        }
    }

    /// 关闭：逐个 `dispose`。
    pub fn dispose_each(&mut self) {
        let keys: Vec<DefaultKey> = self.services.keys().collect();
        for key in keys {
            if let Some(svc) = self.services.get_mut(key) {
                svc.as_service().dispose();
            }
        }
    }
}

impl Default for ServiceRegistry {
    fn default() -> Self {
        Self::new()
    }
}
