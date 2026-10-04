//! [`Scope::effect`] — the teardown contract (ROADMAP D.3).
//!
//! Disposers registered against a scope run in **reverse order**, each carrying
//! a mandatory label. That is what makes "what will be torn down, and in what
//! order?" answerable *before* anything is torn down.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use tracing::{error, info};

/// Monotonic source of effect ids. Process-local, so ids need not be stable.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

type Disposer = Box<dyn FnOnce() + Send>;
type Shared = Arc<Mutex<Vec<Entry>>>;

/// One registered teardown step, flattened for printing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectMeta {
    /// Mandatory human label, e.g. `"wgpu texture atlas"`.
    pub label: String,
    /// Nested scopes. The host currently registers a flat list per fiber, so
    /// this is empty until capability groups introduce nesting.
    pub children: Vec<EffectMeta>,
}

impl fmt::Display for EffectMeta {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label)?;
        for child in &self.children {
            write!(f, "\n  {}", child)?;
        }
        Ok(())
    }
}

/// Registration error.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ScopeError {
    /// The scope is not accepting registrations (it is unloading, disposed or
    /// failed). Register during `load`, never during `tick`.
    #[error("effect registration rejected: scope is not active")]
    InactiveEffect,

    /// The label was empty, which makes a teardown tree unreadable.
    #[error("effect label must not be empty")]
    EmptyLabel,
}

struct Entry {
    id: u64,
    label: String,
    disposer: Option<Disposer>,
}

/// A handle to one registered disposer.
///
/// Dropping the token runs the disposer immediately if it has not run yet;
/// dropping the whole [`Scope`] runs whatever is left, in LIFO order.
///
/// Bind it to a variable that lives as long as you want the effect to.
#[must_use = "dropping the token runs the disposer immediately; bind it to keep the effect alive for the scope's lifetime"]
pub struct EffectToken {
    shared: Shared,
    id: u64,
}

impl fmt::Debug for EffectToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EffectToken").field("id", &self.id).finish()
    }
}

impl Drop for EffectToken {
    fn drop(&mut self) {
        take_and_run(&self.shared, self.id);
    }
}

/// A teardown scope: an ordered list of labeled disposers.
///
/// Cloning shares the same underlying list, which is how a fiber hands handles
/// to the frame loop while keeping registration on its own stack.
#[derive(Clone, Default)]
pub struct Scope {
    shared: Shared,
    /// Whether new effects may be registered.
    open: Arc<AtomicBool>,
}

impl fmt::Debug for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Disposers are opaque closures, so print the labels — which is the
        // only part a human can act on anyway.
        f.debug_struct("Scope")
            .field("open", &self.open.load(Ordering::SeqCst))
            .field("pending", &self.pending())
            .field("effects", &self.effects())
            .finish()
    }
}

impl Scope {
    /// Create a scope that accepts registrations.
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Mutex::new(Vec::new())),
            open: Arc::new(AtomicBool::new(true)),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Entry>> {
        // A poisoned mutex means some disposer panicked while holding it; the
        // entries are still usable, so recover rather than propagate.
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Register `disposer` to run when this scope dies, labelled `label`.
    ///
    /// Disposal happens in **reverse** registration order (LIFO): a resource
    /// allocated before B is released after B — textures before the device,
    /// listeners before the bus.
    ///
    /// Only `FnOnce` is accepted, so double-dispose is impossible by
    /// construction.
    pub fn effect(
        &self,
        label: &str,
        disposer: impl FnOnce() + Send + 'static,
    ) -> Result<EffectToken, ScopeError> {
        if label.is_empty() {
            return Err(ScopeError::EmptyLabel);
        }
        if !self.open.load(Ordering::SeqCst) {
            return Err(ScopeError::InactiveEffect);
        }
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        self.lock().push(Entry {
            id,
            label: label.to_string(),
            disposer: Some(Box::new(disposer)),
        });
        Ok(EffectToken {
            shared: Arc::clone(&self.shared),
            id,
        })
    }

    /// Everything currently registered, for `releste dev --inspect-plugins`.
    pub fn effects(&self) -> Vec<EffectMeta> {
        self.lock()
            .iter()
            .map(|e| EffectMeta {
                label: e.label.clone(),
                children: Vec::new(),
            })
            .collect()
    }

    /// Number of disposers that have not run yet.
    pub fn pending(&self) -> usize {
        self.lock().iter().filter(|e| e.disposer.is_some()).count()
    }

    /// Close the scope to further registration (called when a fiber unloads).
    ///
    /// Idempotent.
    pub fn close(&self) {
        self.open.store(false, Ordering::SeqCst);
    }

    /// Run every remaining disposer in **LIFO** order and clear the list.
    ///
    /// A disposer that fails is logged and skipped; the rest still run. This is
    /// idempotent — a second call is a no-op.
    pub fn dispose_all(&self) {
        self.open.store(false, Ordering::SeqCst);
        let taken = {
            let mut entries = self.lock();
            let mut out = Vec::with_capacity(entries.len());
            for e in entries.iter_mut().rev() {
                if let Some(d) = e.disposer.take() {
                    out.push((e.label.clone(), d));
                }
            }
            entries.clear();
            out
        };
        for (label, d) in taken {
            run_disposer(&label, d);
        }
    }
}

/// Run and remove one entry by id, if it is still pending.
fn take_and_run(shared: &Shared, id: u64) {
    let taken = {
        let mut entries = shared.lock().unwrap_or_else(|e| e.into_inner());
        let Some(pos) = entries.iter().position(|e| e.id == id) else {
            return;
        };
        // Remove as well as take: an effect that has already run is neither
        // pending nor part of the teardown tree any more.
        let entry = entries.remove(pos);
        entry.disposer.map(|d| (entry.label, d))
    };
    if let Some((label, d)) = taken {
        run_disposer(&label, d);
    }
}

/// Run one disposer, containing any panic it raises.
///
/// A single bad plugin must not abort teardown of the rest (D.3).
fn run_disposer(label: &str, d: Disposer) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(d));
    if let Err(payload) = result {
        let msg = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-string panic payload".to_string());
        error!(target: "plugins", "disposer failed: {label}: {msg}");
    } else {
        info!(target: "plugins", "torn down: {label}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn scope_disposal_is_lifo() {
        let scope = Scope::new();
        let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));

        // The tokens must be held: dropping one disposes that effect at once.
        let mut tokens = Vec::new();
        for label in ["first", "second", "third"] {
            let o = Arc::clone(&order);
            tokens.push(
                scope
                    .effect(label, move || {
                        o.lock().unwrap().push(label);
                    })
                    .expect("register"),
            );
        }
        assert_eq!(scope.pending(), 3);

        scope.dispose_all();
        assert_eq!(scope.pending(), 0);

        // Reverse of registration order.
        assert_eq!(*order.lock().unwrap(), vec!["third", "second", "first"]);

        // Spent tokens are inert: dropping them must not re-run anything.
        drop(tokens);
        assert_eq!(order.lock().unwrap().len(), 3);
    }

    #[test]
    fn failing_disposer_is_skipped_and_the_rest_run() {
        let scope = Scope::new();
        let ran = Arc::new(AtomicUsize::new(0));

        let _ok1 = scope
            .effect("ok-1", {
                let r = Arc::clone(&ran);
                move || {
                    r.fetch_add(1, Ordering::SeqCst);
                }
            })
            .unwrap();
        let _boom = scope.effect("boom", || panic!("deliberate")).unwrap();
        let _ok2 = scope
            .effect("ok-2", {
                let r = Arc::clone(&ran);
                move || {
                    r.fetch_add(1, Ordering::SeqCst);
                }
            })
            .unwrap();

        // Nothing has run yet: the tokens are still held.
        assert_eq!(ran.load(Ordering::SeqCst), 0);

        scope.dispose_all();
        assert_eq!(
            ran.load(Ordering::SeqCst),
            2,
            "the panicking disposer did not stop the others"
        );
    }

    #[test]
    fn registration_is_rejected_after_close() {
        let scope = Scope::new();
        scope.close();
        assert!(matches!(
            scope.effect("late", || {}),
            Err(ScopeError::InactiveEffect)
        ));
    }

    #[test]
    fn empty_label_is_rejected() {
        let scope = Scope::new();
        assert!(matches!(
            scope.effect("", || {}),
            Err(ScopeError::EmptyLabel)
        ));
    }

    #[test]
    fn a_disposed_effect_leaves_the_teardown_tree() {
        let scope = Scope::new();
        let t = scope.effect("transient", || {}).unwrap();
        assert_eq!(scope.effects().len(), 1);
        drop(t);
        assert_eq!(scope.effects().len(), 0);
    }

    #[test]
    fn token_drop_runs_its_own_disposer_once() {
        let scope = Scope::new();
        let ran = Arc::new(AtomicUsize::new(0));
        let token = {
            let r = Arc::clone(&ran);
            scope
                .effect("early", move || {
                    r.fetch_add(1, Ordering::SeqCst);
                })
                .unwrap()
        };
        drop(token);
        assert_eq!(ran.load(Ordering::SeqCst), 1);
        // scope disposal must not run it a second time
        scope.dispose_all();
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn effects_are_printable() {
        let scope = Scope::new();
        let _t1 = scope.effect("wgpu texture atlas", || {}).unwrap();
        let _t2 = scope.effect("device", || {}).unwrap();
        let tree = scope.effects();
        assert_eq!(tree.len(), 2);
        assert_eq!(tree[0].label, "wgpu texture atlas");
        assert!(tree[0].children.is_empty());
    }
}
