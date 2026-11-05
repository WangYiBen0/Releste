//! Service trait, ServiceRegistry, Context.

use std::any::{Any, TypeId};
use std::collections::HashMap;

use slotmap::{DefaultKey, SlotMap};

use crate::event::EventBus;
use crate::frame::FrameId;
use releste_math::Fx;

/// Unique identifier for a Service.
///
/// Declared by the service itself (such as `ServiceId::new("input")`) for
/// logging and debugging.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ServiceId(&'static str);

impl ServiceId {
    /// Construct.
    pub const fn new(name: &'static str) -> Self {
        ServiceId(name)
    }

    /// Name.
    pub const fn name(self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for ServiceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// Engine service.
///
/// All systems (simulation, rendering, audio, ...) implement this trait.
pub trait Service: 'static {
    /// Service ID (declared by itself, globally unique).
    fn id(&self) -> ServiceId;

    /// Initialization: register event subscriptions and acquire references to
    /// other services here.
    fn attach(&mut self, ctx: &mut Context);

    /// Update every frame. `dt` is always [`FIXED_DT`](releste_math::FIXED_DT).
    fn update(&mut self, ctx: &mut Context, frame: FrameId, dt: Fx);

    /// Release resources.
    fn dispose(&mut self) {}
}

/// Service runtime context.
///
/// Provides event bus access. Dependencies between Services are injected
/// explicitly from the outside during the `attach` phase.
pub struct Context<'a> {
    pub(crate) bus: &'a mut EventBus,
}

impl<'a> Context<'a> {
    /// Construct a context.
    ///
    /// Mainly for tests and embedded hosts; the normal main loop creates it
    /// via [`Kernel`](crate::Kernel).
    pub fn new(bus: &'a mut EventBus) -> Self {
        Context { bus }
    }

    /// Broadcast an event.
    pub fn send<T: Any + Send>(&self, event: T) {
        self.bus.send(event);
    }

    /// Register an event subscription.
    ///
    /// Each event type can have only one subscriber; a duplicate subscription
    /// returns
    /// [`SubscribeError::AlreadySubscribed`](crate::event::SubscribeError::AlreadySubscribed).
    pub fn subscribe<T: Any + Send>(
        &mut self,
    ) -> Result<flume::Receiver<T>, crate::event::SubscribeError> {
        self.bus.with_subscriber()
    }
}

/// Service registry.
///
/// Holds all services and supports lookup by type and by [`ServiceId`].
pub struct ServiceRegistry {
    services: SlotMap<DefaultKey, Box<dyn AnyService>>,
    by_type: HashMap<TypeId, DefaultKey>,
    by_name: HashMap<ServiceId, DefaultKey>,
}

/// Type-erased service.
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
    /// Create an empty registry.
    pub fn new() -> Self {
        ServiceRegistry {
            services: SlotMap::with_key(),
            by_type: HashMap::new(),
            by_name: HashMap::new(),
        }
    }

    /// Register a service and return its [`ServiceId`].
    ///
    /// If a service with the same name is already registered, returns
    /// `Err(existing_id)`.
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

    /// Get a service reference.
    pub fn get<S: Service>(&self) -> Option<&S> {
        self.by_type
            .get(&TypeId::of::<S>())
            .and_then(|k| self.services.get(*k))
            .and_then(|s| s.as_any().downcast_ref::<S>())
    }

    /// Get a mutable service reference.
    pub fn get_mut<S: Service>(&mut self) -> Option<&mut S> {
        self.by_type
            .get(&TypeId::of::<S>())
            .and_then(|k| self.services.get_mut(*k))
            .and_then(|s| s.as_any_mut().downcast_mut::<S>())
    }

    /// Get a mutable reference by [`ServiceId`] (type-erased).
    pub fn get_by_id_mut(&mut self, id: ServiceId) -> Option<&mut dyn Service> {
        self.by_name
            .get(&id)
            .and_then(|k| self.services.get_mut(*k))
            .map(|s| s.as_service())
    }

    /// Whether it is already registered.
    pub fn contains(&self, id: ServiceId) -> bool {
        self.by_name.contains_key(&id)
    }

    /// All registered service IDs.
    pub fn ids(&self) -> Vec<ServiceId> {
        self.by_name.keys().copied().collect()
    }

    /// Every frame: update all services one by one.
    ///
    /// Collect the keys first and then `get_mut` one at a time, avoiding the
    /// borrow conflict of mutable iteration while holding an immutable
    /// reference to the registry.
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

    /// Boot phase: `attach` all services one by one.
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

    /// Shut down: `dispose` one by one.
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
