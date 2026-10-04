//! Capability registry: what a plugin provides, and what depends on it.
//!
//! Dependencies are declared as **data**, never resolved by an import — that is
//! the whole point of hot-plug. Losing a provider rewrites an epoch string;
//! any fiber whose epoch changed unloads, and comes back when the provider
//! does (ROADMAP D.4).

use std::collections::BTreeMap;

/// A capability that one plugin provides and others may `inject`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceEntry {
    /// Plugin that provides it.
    pub provider: String,
    /// Monotonic uid. Changes on every re-provide, which is what folds into an
    /// epoch.
    pub uid: u64,
}

/// One entity kind supplied by a plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityKind {
    /// Kind name as written in a `.map`, e.g. `"crystal"`.
    pub name: String,
    /// `u32::from_le(blake3(name)[0..4])` — the id a `.map` resolves against.
    pub kind_id: u32,
    /// JSON-encoded schema, so the host never has to link a plugin's Rust types.
    pub schema: String,
    /// Plugin that registered it.
    pub provider: String,
}

/// Result of asking the registry whether a set of dependencies is satisfied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dependency {
    /// Every dependency is present. The string is the epoch, e.g. `"render:3"`.
    Ready(String),
    /// At least one dependency is neither present nor loadable.
    Missing(String),
}

/// The host-side capability registry.
#[derive(Debug, Default)]
pub struct Registry {
    services: BTreeMap<String, ServiceEntry>,
    kinds: BTreeMap<u32, EntityKind>,
    next_uid: u64,
}

impl Registry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    fn bump(&mut self) -> u64 {
        self.next_uid += 1;
        self.next_uid
    }

    /// Publish `service`, provided by `provider`.
    ///
    /// Re-providing bumps the uid, which changes every dependent epoch.
    pub fn provide(&mut self, service: impl Into<String>, provider: impl Into<String>) -> u64 {
        let uid = self.bump();
        self.services.insert(
            service.into(),
            ServiceEntry {
                provider: provider.into(),
                uid,
            },
        );
        uid
    }

    /// Withdraw a single service by name. Unknown names are ignored.
    pub fn revoke(&mut self, service: &str) {
        self.services.remove(service);
    }

    /// Withdraw every service and entity kind published by `provider`.
    ///
    /// Called at the unload boundary; dependents see the epoch change and go
    /// `Inactive` instead of dangling.
    pub fn revoke_provider(&mut self, provider: &str) {
        self.services.retain(|_, v| v.provider != provider);
        self.kinds.retain(|_, v| v.provider != provider);
    }

    /// Provider uid of `service`, if any.
    pub fn uid_of(&self, service: &str) -> Option<u64> {
        self.services.get(service).map(|v| v.uid)
    }

    /// Whether `service` is currently provided, and by whom.
    pub fn provider_of(&self, service: &str) -> Option<&str> {
        self.services.get(service).map(|v| v.provider.as_str())
    }

    /// Compute the epoch for `deps`.
    ///
    /// Returns [`Dependency::Missing`] with the first absent name as soon as
    /// any dependency is unavailable — a fiber with an unsatisfiable dependency
    /// is `FAILED`, not dangling.
    pub fn epoch(&self, deps: &[String]) -> Dependency {
        let mut out = String::new();
        for (i, dep) in deps.iter().enumerate() {
            let Some(uid) = self.uid_of(dep) else {
                return Dependency::Missing(dep.clone());
            };
            if i > 0 {
                out.push(':');
            }
            out.push_str(dep);
            out.push(':');
            out.push_str(&uid.to_string());
        }
        Dependency::Ready(out)
    }

    /// Publish an entity kind. Replaces any prior registration under the same id.
    pub fn register_entity_kind(&mut self, kind: EntityKind) {
        self.kinds.insert(kind.kind_id, kind);
    }

    /// Resolve an entity kind id to its definition.
    ///
    /// A `.map` may reference a kind no plugin currently provides — the caller
    /// must warn and skip, never panic (AGENTS.md §15.8/§15.9).
    pub fn entity_kind(&self, kind_id: u32) -> Option<&EntityKind> {
        self.kinds.get(&kind_id)
    }

    /// All registered entity kinds, ordered by id.
    pub fn entity_kinds(&self) -> impl Iterator<Item = &EntityKind> {
        self.kinds.values()
    }

    /// All provided services, ordered by name.
    pub fn services(&self) -> impl Iterator<Item = (&str, &ServiceEntry)> {
        self.services.iter().map(|(k, v)| (k.as_str(), v))
    }
}

/// The canonical kind id: first four bytes of `blake3(name)`, little-endian.
///
/// Stable across plugins and runs — it does not care *which* plugin supplies
/// the kind (ROADMAP 3.4).
pub fn kind_id_for(name: &str) -> u32 {
    let hash = blake3::hash(name.as_bytes());
    u32::from_le_bytes(hash.as_bytes()[..4].try_into().expect("4 bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_tracks_provider_changes() {
        let mut r = Registry::new();
        assert_eq!(
            r.epoch(&["render".into()]),
            Dependency::Missing("render".into())
        );

        let uid = r.provide("render", "render-plugin");
        assert_eq!(
            r.epoch(&["render".into()]),
            Dependency::Ready(format!("render:{uid}"))
        );

        // Re-provide from a new version of the plugin -> uid bumps -> epoch moves.
        let uid2 = r.provide("render", "render-plugin");
        assert!(uid2 > uid);
        assert_ne!(
            r.epoch(&["render".into()]),
            Dependency::Ready(format!("render:{uid}"))
        );
    }

    #[test]
    fn revoking_a_provider_drops_everything_it_published() {
        let mut r = Registry::new();
        r.provide("render", "p");
        r.register_entity_kind(EntityKind {
            name: "crystal".into(),
            kind_id: kind_id_for("crystal"),
            schema: "{}".into(),
            provider: "p".into(),
        });
        assert!(r.entity_kind(kind_id_for("crystal")).is_some());

        r.revoke_provider("p");
        assert!(r.uid_of("render").is_none());
        assert!(r.entity_kind(kind_id_for("crystal")).is_none());
    }

    #[test]
    fn kind_id_is_stable_and_name_derived() {
        let a = kind_id_for("crystal");
        assert_eq!(a, kind_id_for("crystal"));
        assert_ne!(a, kind_id_for("wall"));
        // first 4 bytes of blake3, little-endian
        let expected =
            u32::from_le_bytes(blake3::hash(b"crystal").as_bytes()[..4].try_into().unwrap());
        assert_eq!(a, expected);
    }

    #[test]
    fn epoch_of_no_deps_is_empty_and_ready() {
        let r = Registry::new();
        assert_eq!(r.epoch(&[]), Dependency::Ready(String::new()));
    }
}
