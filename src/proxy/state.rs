//! Shared state every proxy connection's handlers read from.
//!
//! Cloned-and-`Arc`-wrapped fields mirror the relevant slots of
//! [`crate::ipc::methods::MethodContext`] — the catalog, the
//! supervisor registry, persisted state, and the launch env — so
//! the proxy can answer requests without round-tripping through the
//! IPC dispatcher. Unit 1 only consumes `catalog` and `supervisors`
//! (for `/health`'s `models_loaded` / `models_discovered` counts);
//! later units lean on the rest.

use std::sync::Arc;

use crate::daemon::registry::SupervisorRegistry;
use crate::discovery::ModelCatalog;
use crate::ipc::methods::{LaunchEnv, MethodContext, PersistedState};

/// Cheap-to-clone bundle of the daemon-side handles the proxy needs.
/// The inner `Arc` makes per-connection cloning a single refcount
/// bump — the `service_fn` closure clones a fresh handle for every
/// inbound HTTP connection so handler futures don't borrow across
/// scheduler boundaries.
#[derive(Clone)]
pub struct ProxyState {
  /// Discovered models. Unit 2's `/v1/models` reads from this; Unit 3's
  /// name resolution builds `CatalogRow`s from a snapshot for the
  /// fuzzy resolver.
  pub catalog: ModelCatalog,
  /// Live supervisor map. Unit 3 looks up `(ModelId → port)` here;
  /// Unit 4 walks it for the family-MRU fallback choice. Unit 1
  /// reads only `len()` for `/health` counts.
  pub supervisors: SupervisorRegistry,
  /// Persisted favorites / presets / last_params / running. Unit 4's
  /// auto-start composes `LaunchParams` from `last_params` before
  /// falling through to `arch_defaults`.
  pub state: PersistedState,
  /// Binary path + port range + log dir + probe options + arch_defaults.
  /// `None` in tests that never launch; Unit 4 refuses to auto-start
  /// in that case.
  pub launch: Option<LaunchEnv>,
}

impl ProxyState {
  /// Project the relevant fields out of an existing [`MethodContext`].
  /// The proxy task receives this handle from `run_foreground` after
  /// the rest of the daemon context has been assembled.
  pub fn from_context(ctx: &MethodContext) -> Arc<Self> {
    Arc::new(Self {
      catalog: ctx.catalog.clone(),
      supervisors: ctx.supervisors.clone(),
      state: ctx.state.clone(),
      launch: ctx.launch.clone(),
    })
  }
}
