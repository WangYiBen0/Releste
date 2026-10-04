//! The [`Fiber`] state machine.
//!
//! States are adopted verbatim from Cordis' `FiberState`, because they already
//! encode the cases that matter for hot-plug (ROADMAP D.2).

/// Lifecycle state of one plugin instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FiberState {
    /// Created, not yet loaded.
    Pending,
    /// `load()` is running.
    Loading,
    /// Loaded and receiving ticks.
    Active,
    /// Revoked at a frame boundary; disposers are draining.
    Unloading,
    /// Terminal: released.
    Disposed,
    /// Load threw. Logged and skipped — it never takes the host down, and it
    /// never retries in a loop. [`Fiber::recover`] is the only way out.
    Failed,
}

impl FiberState {
    /// The Cordis transition table, applied by the host.
    ///
    /// Returns `true` when the move is legal.
    pub const fn can_transition(self, next: FiberState) -> bool {
        use FiberState::*;
        matches!(
            (self, next),
            (Pending, Loading)
                | (Loading, Active)
                | (Loading, Failed)
                | (Active, Unloading)
                | (Unloading, Disposed)
                | (Unloading, Failed)
                // `update()` is the single documented path out of FAILED (D.10).
                | (Failed, Loading)
                // A fresh instance may be created directly into FAILED when the
                // manifest or artifact is rejected before load is attempted.
                | (Pending, Failed)
        )
    }

    /// Whether the fiber still owns its capabilities.
    pub const fn is_live(self) -> bool {
        matches!(self, FiberState::Loading | FiberState::Active)
    }
}

/// One plugin instance.
#[derive(Debug, Clone)]
pub struct Fiber {
    name: String,
    state: FiberState,
    /// Why the fiber is `Failed`, if it is.
    reason: Option<String>,
}

impl Fiber {
    /// Create a fiber in [`FiberState::Pending`].
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            state: FiberState::Pending,
            reason: None,
        }
    }

    /// Plugin name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Current state.
    pub fn state(&self) -> FiberState {
        self.state
    }

    /// Failure reason, present only when [`Fiber::state`] is
    /// [`FiberState::Failed`].
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    /// Move to `next`, or return the current state unchanged if the edge is
    /// not in the table.
    ///
    /// Never panics (AGENTS.md §15.8).
    pub fn advance(&mut self, next: FiberState) -> Result<(), FiberState> {
        if self.state.can_transition(next) {
            self.state = next;
            Ok(())
        } else {
            Err(self.state)
        }
    }

    /// Mark the fiber `Failed` with a human-readable reason.
    pub fn fail(&mut self, reason: impl Into<String>) {
        self.reason = Some(reason.into());
        // FAILED is reachable from every live state; forcing it here keeps a
        // failure path from being swallowed by a table lookup.
        self.state = FiberState::Failed;
    }

    /// The only way out of `FAILED` (D.10): clears the reason and returns the
    /// fiber to `Pending` so `load()` may be attempted again.
    pub fn recover(&mut self) {
        if self.state == FiberState::Failed {
            self.reason = None;
            self.state = FiberState::Pending;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_reaches_active() {
        let mut f = Fiber::new("render");
        assert_eq!(f.state(), FiberState::Pending);
        assert!(f.advance(FiberState::Loading).is_ok());
        assert!(f.advance(FiberState::Active).is_ok());
        assert!(f.state().is_live());
    }

    #[test]
    fn cannot_skip_loading() {
        let mut f = Fiber::new("render");
        assert_eq!(f.advance(FiberState::Active), Err(FiberState::Pending));
        assert_eq!(f.state(), FiberState::Pending);
    }

    #[test]
    fn failed_recovers_only_via_update_or_recover() {
        let mut f = Fiber::new("bad");
        f.fail("dlopen: symbol not found");
        assert_eq!(f.state(), FiberState::Failed);
        assert_eq!(f.reason(), Some("dlopen: symbol not found"));

        // You may not skip straight back to ACTIVE.
        assert_eq!(f.advance(FiberState::Active), Err(FiberState::Failed));

        // FAILED -> LOADING is `update()`, the one documented retry path.
        assert!(f.advance(FiberState::Loading).is_ok());
        assert!(f.advance(FiberState::Active).is_ok());
    }

    #[test]
    fn recover_returns_a_failed_fiber_to_pending() {
        let mut f = Fiber::new("bad");
        f.fail("missing entry symbol");
        f.recover();
        assert_eq!(f.state(), FiberState::Pending);
        assert!(f.reason().is_none());
        assert!(f.advance(FiberState::Loading).is_ok());
        // recover() on a healthy fiber is a no-op
        f.recover();
        assert_eq!(f.state(), FiberState::Loading);
    }

    #[test]
    fn unloading_terminates() {
        let mut f = Fiber::new("x");
        f.advance(FiberState::Loading).unwrap();
        f.advance(FiberState::Active).unwrap();
        f.advance(FiberState::Unloading).unwrap();
        assert!(!f.state().is_live());
        assert!(f.advance(FiberState::Disposed).is_ok());
        assert_eq!(f.state(), FiberState::Disposed);
        // DISPOSED is terminal
        assert!(f.advance(FiberState::Loading).is_err());
    }
}
