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
use std::time::Duration;

use crate::daemon::registry::SupervisorRegistry;
use crate::discovery::ModelCatalog;
use crate::ipc::methods::{LaunchEnv, MethodContext, PersistedState};

use super::coalesce::Coalesce;
use super::mru::MruTracker;

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
  /// Pooled HTTP client used by Unit 3's forwarding path. One per
  /// proxy process — hyper handles keep-alive per-host inside the
  /// pool, so a second client would just be cargo cult. Wrapped in
  /// `Arc` so the per-connection clone is a refcount bump rather
  /// than rebuilding the pool.
  pub http_client: Arc<reqwest::Client>,
  /// Single-flight coalesce map for Unit 4's auto-start path. Keyed
  /// on [`crate::gguf::identity::ModelId`] so two concurrent
  /// requests with different fuzzy spellings of the same model
  /// share one launch.
  pub(crate) coalesce: Coalesce,
  /// In-memory `last_request_at` tracker. Unit 4's family-MRU
  /// fallback picker reads from it; `route::forward_request` writes
  /// to it as forwarding starts (per the plan's "as it starts
  /// forwarding, not on completion" rule).
  pub(crate) mru: MruTracker,
  /// Full IPC context handle. Cheap to clone (every field is
  /// already `Arc`-wrapped) and only consumed by the proxy's
  /// auto-start path which calls into
  /// [`crate::ipc::methods::start_model_inner`]. Kept off the hot
  /// path — Ready-model forwarding never reads from this field.
  pub(crate) ctx: MethodContext,
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
      http_client: Arc::new(build_http_client()),
      coalesce: Coalesce::new(),
      mru: MruTracker::new(),
      ctx: ctx.clone(),
    })
  }
}

/// Build the proxy's pooled HTTP client. Single source so tests can
/// reach into the same pool if they ever need to. The settings here
/// target the loopback `llama-server` upstream: short-ish connect
/// timeout (the child is on the same machine, anything > 5 s is a
/// real bug), no request timeout (chat completions are arbitrarily
/// long-running by design), pooling kept on so repeated requests
/// against the same port reuse keep-alive.
fn build_http_client() -> reqwest::Client {
  reqwest::Client::builder()
    .connect_timeout(Duration::from_secs(5))
    .pool_idle_timeout(Duration::from_secs(90))
    .build()
    // Builder failures here would be a misconfigured TLS stack /
    // missing certificate root. Loopback HTTP has none of those — we
    // never hit a network. If this ever panics in production the
    // build is broken, not the runtime.
    .expect("reqwest client must build on a healthy runtime")
}
