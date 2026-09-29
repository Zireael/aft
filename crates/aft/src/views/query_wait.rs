//! One total query-wait budget, measured against installed data rather than a pointer.

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::contracts::{QueryWait, ViewAccess, WaitOutcome};
use super::first_load::QueryState;
use crate::blob_store::v2::FamilyPlane;

pub struct BoundedQueryWait {
    state: Arc<dyn QueryState>,
}

impl BoundedQueryWait {
    pub fn new(state: Arc<dyn QueryState>) -> Self {
        Self { state }
    }
}

impl QueryWait for BoundedQueryWait {
    fn wait_for(&self, access: &ViewAccess, plane: FamilyPlane, budget: Duration) -> WaitOutcome {
        let started = Instant::now();
        loop {
            let (snapshot, mut unreflected) = self.state.installed_state(access, plane);
            if unreflected.is_empty() {
                return WaitOutcome::Installed(snapshot);
            }
            let remaining = budget.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                unreflected.sort();
                unreflected.dedup();
                return WaitOutcome::TimedOut {
                    snapshot,
                    unreflected,
                };
            }
            // Edits do not move the deadline. Always probe again after waking,
            // so both success and timeout return a post-wait installed snapshot.
            std::thread::sleep(remaining.min(Duration::from_millis(10)));
        }
    }
}

/// Explicit opt-in binding for a checkout's query runtime. Configuring the
/// legacy runtime never constructs this value; cutover supplies it per root.
pub struct CheckoutQueryRuntime {
    pub access: ViewAccess,
    pub waiter: Arc<dyn QueryWait>,
    pub callgraph: Arc<super::callgraph::CallgraphPlane>,
}

impl CheckoutQueryRuntime {
    pub fn new(
        access: ViewAccess,
        state: Arc<dyn QueryState>,
        callgraph: Arc<super::callgraph::CallgraphPlane>,
    ) -> Self {
        Self {
            access,
            waiter: Arc::new(BoundedQueryWait::new(state)),
            callgraph,
        }
    }
}
