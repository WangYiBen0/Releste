//! 事件总线。
//!
//! 类型化广播：每种事件类型 `T` 有一个 sender。订阅者获得 `Receiver<T>`。
//! 若同类型有多个订阅者，需由上游 service 转发。

use std::any::{Any, TypeId};
use std::collections::HashMap;

use flume::Sender;

/// 事件订阅错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SubscribeError {
    /// 该事件类型已经有订阅者。
    ///
    /// 事件总线是**单订阅者**模型；需要多播请在上层转发。
    #[error("event type `{type_name}` already has a subscriber")]
    AlreadySubscribed {
        /// 发生冲突的事件类型名。
        type_name: &'static str,
    },
}

/// 事件总线 — 类型化事件广播。
///
/// 每种事件类型只有 **一个** 订阅者。若需多播，
/// 由服务内部转发。
pub struct EventBus {
    channels: HashMap<TypeId, Box<dyn AnyChannel>>,
}

struct TypedChannel<T: Any + Send> {
    tx: Sender<T>,
}

trait AnyChannel: Send {
    fn as_any(&self) -> &dyn Any;
}

impl<T: Any + Send> AnyChannel for TypedChannel<T> {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl EventBus {
    /// 空总线。
    pub fn new() -> Self {
        EventBus {
            channels: HashMap::new(),
        }
    }

    /// 注册事件类型 `T` 并返回 Receiver。
    ///
    /// 每种类型只能注册一次；重复注册返回
    /// [`SubscribeError::AlreadySubscribed`]。
    pub fn with_subscriber<T: Any + Send>(&mut self) -> Result<flume::Receiver<T>, SubscribeError> {
        let type_id = TypeId::of::<T>();
        if self.channels.contains_key(&type_id) {
            return Err(SubscribeError::AlreadySubscribed {
                type_name: std::any::type_name::<T>(),
            });
        }
        let (tx, rx) = flume::unbounded();
        self.channels
            .insert(type_id, Box::new(TypedChannel::<T> { tx }));
        Ok(rx)
    }

    /// 发送事件。若无该类型的订阅者，事件被丢弃。
    pub fn send<T: Any + Send>(&self, event: T) {
        let type_id = TypeId::of::<T>();
        if let Some(ch) = self.channels.get(&type_id) {
            let tc: &TypedChannel<T> = ch
                .as_any()
                .downcast_ref::<TypedChannel<T>>()
                .expect("type id matches but downcast failed");
            let _ = tc.tx.send(event);
        }
    }

    /// 移除某类型的订阅者。
    pub fn remove<T: Any + Send>(&mut self) {
        self.channels.remove(&TypeId::of::<T>());
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_receive() {
        let mut bus = EventBus::new();
        let rx = bus.with_subscriber::<i32>().unwrap();
        bus.send(42i32);
        assert_eq!(rx.try_recv().unwrap(), 42);
    }

    #[test]
    fn no_subscriber_no_panic() {
        let bus = EventBus::new();
        bus.send("hello");
    }

    #[test]
    fn duplicate_subscriber_reports_the_type() {
        let mut bus = EventBus::new();
        let _rx1 = bus.with_subscriber::<i32>().unwrap();
        match bus.with_subscriber::<i32>() {
            Err(SubscribeError::AlreadySubscribed { type_name }) => {
                assert!(type_name.contains("i32"), "got {type_name}");
            }
            other => panic!("expected AlreadySubscribed, got {other:?}"),
        }
    }

    #[test]
    fn different_types_can_coexist() {
        let mut bus = EventBus::new();
        let _a = bus.with_subscriber::<i32>().unwrap();
        let _b = bus.with_subscriber::<String>().unwrap();
        bus.send(1i32);
        bus.send("hello".to_string());
    }
}
