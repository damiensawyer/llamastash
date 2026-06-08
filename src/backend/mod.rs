//! The `Backend` seam: everything currently hardwired to `llama-server`
//! on the **launch / supervise / identify** side, expressed as a
//! contract so other inference engines can plug in.
//!
//! Phase 1 (this module + [`llama_cpp`]) ships the contract with
//! llama.cpp as the **sole reference implementation** and **zero
//! user-visible behavior change**. The generic process-supervision
//! machinery in [`crate::daemon::supervisor`] (state machine, log
//! rotation, ring buffer, resource sampler, exit watcher, signal
//! handling) stays put — only the four llama.cpp-specific spots move
//! behind this seam: argv composition, the `LLAMA_ARG_*` / `HF_*` env
//! strip, the readiness endpoint, and identity.
//!
//! See `docs/plans/2026-06-08-001-refactor-backend-trait-abstraction-plan.md`
//! and the origin brainstorm
//! `docs/brainstorms/2026-06-08-multi-backend-abstraction-requirements.md`.
//!
//! # Two lifecycle shapes (R2)
//!
//! The contract must not assume **one process per model**. Two shapes
//! exist:
//!
//! - **Process-per-model** (llama.cpp — Phase 1): llamastash spawns one
//!   `llama-server` per model and owns its full lifecycle. The launch
//!   produces a [`LaunchPlan::SpawnProcess`].
//! - **Managed-multiplexer** (Lemonade — Phase 2): llamastash supervises
//!   one long-lived `lemond` and delegates per-model start/stop/list to
//!   its API. That launch would produce a `LaunchPlan::DelegateToManager`
//!   arm (not built yet) — additive to the enum, so adding it does not
//!   change [`Backend::prepare_launch`]'s signature.
//!
//! # Design gate — how Lemonade would implement this contract
//!
//! Validated on paper (per the plan) so the trait doesn't grow
//! process-per-model / local-GGUF assumptions while only llama.cpp is
//! built:
//!
//! - [`Backend::id`] → `"lemonade"`. Trivial.
//! - [`Backend::lifecycle`] → [`Lifecycle::ManagedMultiplexer`]. Already
//!   modelled.
//! - [`Backend::capabilities`] → the subset of [`KnobField`]s `lemond`
//!   honors; the rest are dropped + surfaced as unsupported (R6, Phase 2
//!   UI). The capability type already expresses an arbitrary subset.
//! - [`Backend::identify`] → a Lemonade-registry model has **no local
//!   GGUF path or header**. This is the one method whose signature must
//!   change in Phase 2 (the R12 `ModelId` generalisation). Accepted and
//!   pre-acknowledged in the plan — Phase 1 keeps the concrete GGUF
//!   identity rather than doing a speculative `state.json` schema break.
//! - [`Backend::prepare_launch`] → returns the (Phase 2)
//!   `LaunchPlan::DelegateToManager` arm carrying an API start-request,
//!   *not* a process spec. Because translation is pure (no I/O) and the
//!   async API call happens when the plan is *executed*, this method can
//!   stay synchronous for both shapes.
//!
//! The only Phase-2 contract change this walkthrough surfaces is
//! `identify` (the known, accepted `ModelId` generalisation). No method
//! forces a process-per-model assumption. The seam is honest.

pub mod llama_cpp;

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::daemon::probe::ProbeOptions;
use crate::gguf::identity::ModelId;
use crate::launch::flag_aliases::{knob_specs, KnobField};
use crate::launch::params::LaunchParams;

/// How a backend manages the lifecycle of the models it runs (R2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
  /// One supervised child process per model; llamastash owns the full
  /// lifecycle (spawn, probe, evict-by-kill). llama.cpp.
  ProcessPerModel,
  /// One long-lived supervised umbrella process; per-model start/stop
  /// /list delegated to the backend's own API. Lemonade (Phase 2).
  ManagedMultiplexer,
}

impl Lifecycle {
  /// Stable lowercase label for logs / future JSON projection.
  pub fn label(self) -> &'static str {
    match self {
      Lifecycle::ProcessPerModel => "process_per_model",
      Lifecycle::ManagedMultiplexer => "managed_multiplexer",
    }
  }
}

/// How to tell that a launched model is ready to serve.
///
/// Phase 1 has only the HTTP-poll shape (llama.cpp's `/health`). The
/// poll semantics live in [`crate::daemon::probe`]; this declares the
/// endpoint + the status that means "ready" so the probe is no longer
/// hardwired to `/health`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
  /// Poll an HTTP path until it returns `ready_status`. Any other
  /// status (including the conventional `503` "still loading") keeps
  /// the probe waiting until its timeout — matching today's behavior.
  HttpPoll { path: String, ready_status: u16 },
}

/// A fully-resolved instruction for starting one model on a
/// **process-per-model** backend. Everything
/// [`crate::daemon::supervisor::spawn`] needs to launch + probe a child,
/// with no llama.cpp specifics left in the supervisor.
#[derive(Debug, Clone)]
pub struct ProcessLaunchSpec {
  /// The executable to spawn (the device-owning binary, already chosen
  /// by the orchestrator).
  pub binary: PathBuf,
  /// The full argv (everything after the program name). For llama.cpp
  /// this is exactly [`crate::launch::params::compose`]'s output —
  /// pinned by golden parity tests.
  pub argv: Vec<OsString>,
  /// Environment variables to remove before spawn (the loopback /
  /// credential contract: `LLAMA_ARG_*`, `HF_*`). Declared by the
  /// backend rather than hardcoded in the supervisor.
  pub env_remove: Vec<&'static str>,
  /// How to detect readiness once spawned.
  pub readiness: Readiness,
  /// Probe budget (the caller has already applied `scale_for_model`).
  pub probe: ProbeOptions,
}

/// The result of translating the resolved knob IR into "how to start
/// this model" for a given backend.
///
/// Phase 1 only ever constructs [`LaunchPlan::SpawnProcess`]. The
/// managed-multiplexer arm is intentionally absent until Phase 2; adding
/// it is additive and does not change [`Backend::prepare_launch`]'s
/// signature (R2).
#[derive(Debug, Clone)]
pub enum LaunchPlan {
  /// Spawn and supervise a child process (process-per-model shape).
  SpawnProcess(ProcessLaunchSpec),
  // Phase 2 (managed-multiplexer): DelegateToManager(ManagerStartRequest),
}

/// The set of knob IR fields a backend can honor (R6).
///
/// llama.cpp supports every [`KnobField`]. Other backends declare a
/// subset; fields outside the set are dropped from that backend's launch
/// and (Phase 2) surfaced as "not supported by `<backend>`" in Settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnobCapability {
  supported: BTreeSet<KnobField>,
}

impl KnobCapability {
  /// Every knob the typed-knob surface defines — llama.cpp's full
  /// vocabulary, derived from the canonical [`knob_specs`] table so it
  /// can never drift from the flags `compose` actually emits.
  pub fn all() -> Self {
    Self {
      supported: knob_specs().iter().map(|s| s.field).collect(),
    }
  }

  /// Whether this backend honors `field`. Phase 2 backends that honor
  /// only a subset of the IR will construct a narrower set here; the
  /// subset constructor lands with that first real consumer.
  pub fn supports(&self, field: KnobField) -> bool {
    self.supported.contains(&field)
  }
}

/// One inference backend (R1). All behavior currently hardwired to
/// `llama-server` is expressed here so each backend owns its own
/// translation from the neutral knob IR.
///
/// Phase 1 has a single implementor, [`llama_cpp::LlamaCppBackend`].
/// Dispatch is via the [`llama_cpp::Backends`] enum (zero-cost, exhaustive) rather
/// than `dyn Backend` — the backend set is small and closed.
///
/// Every method is synchronous: translation is pure (no I/O), so neither
/// lifecycle shape needs async here. The async work (spawning a process,
/// or calling a multiplexer's API) happens when a [`LaunchPlan`] is
/// *executed*, not when it is built.
pub trait Backend {
  /// Stable backend identifier (`"llamacpp"`). Used by the registry and
  /// any backend-aware surface (R3).
  fn id(&self) -> &'static str;

  /// The lifecycle shape this backend uses (R2).
  fn lifecycle(&self) -> Lifecycle;

  /// Which knob IR fields this backend honors (R6).
  fn capabilities(&self) -> &KnobCapability;

  /// Compute the stable identity for a model handled by this backend.
  ///
  /// Phase 1 (llama.cpp) takes the already-read GGUF header bytes and
  /// returns the concrete `(path, BLAKE3)` [`ModelId`]. Phase 2's
  /// registry-named models have no local header — that generalisation
  /// is the one accepted, pre-flagged signature change (see the
  /// module-level design gate).
  fn identify(&self, path: &Path, header_bytes: &[u8]) -> ModelId;

  /// Translate a fully-resolved [`LaunchParams`] into a [`LaunchPlan`]
  /// (R5). Pure and infallible for llama.cpp — `compose` cannot fail.
  ///
  /// `binary` is the device-owning executable the orchestrator already
  /// selected; `probe` carries the size-scaled budget.
  fn prepare_launch(
    &self,
    params: &LaunchParams,
    port: u16,
    binary: PathBuf,
    probe: ProbeOptions,
  ) -> LaunchPlan;
}

/// Pick the backend that runs the model at `model_path` (R3/R13).
///
/// Selection is automatic from the model's source/format — no user
/// choice in the common case. A GGUF on disk binds to the **direct**
/// llama.cpp backend, never a wrapper, even once other backends exist.
///
/// Phase 1 has a single backend, so this always returns llama.cpp; the
/// `model_path` is the input the rule will key on in Phase 2 (registry-
/// sourced Lemonade models resolve to the Lemonade backend here). This
/// is the one selection seam — adding a backend means adding a variant
/// to [`llama_cpp::Backends`] and a branch here, not editing the
/// supervisor, proxy, or resolver.
pub fn select_backend(_model_path: &std::path::Path) -> llama_cpp::Backends {
  llama_cpp::Backends::LlamaCpp(llama_cpp::LlamaCppBackend::new())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn capability_all_covers_every_knob_spec() {
    let all = KnobCapability::all();
    for spec in knob_specs() {
      assert!(
        all.supports(spec.field),
        "KnobCapability::all() must cover {:?}",
        spec.field
      );
    }
  }

  #[test]
  fn process_launch_spec_is_constructible_and_readable() {
    // Proves the process-per-model shape is usable end-to-end as a
    // value (the supervisor will consume exactly these fields).
    let spec = ProcessLaunchSpec {
      binary: PathBuf::from("/usr/bin/llama-server"),
      argv: vec![OsString::from("--port"), OsString::from("41100")],
      env_remove: vec!["LLAMA_ARG_HOST"],
      readiness: Readiness::HttpPoll {
        path: "/health".to_string(),
        ready_status: 200,
      },
      probe: ProbeOptions::default(),
    };
    match LaunchPlan::SpawnProcess(spec) {
      LaunchPlan::SpawnProcess(s) => {
        assert_eq!(s.binary, PathBuf::from("/usr/bin/llama-server"));
        assert_eq!(s.argv.len(), 2);
        assert_eq!(s.env_remove, vec!["LLAMA_ARG_HOST"]);
        assert!(matches!(
          s.readiness,
          Readiness::HttpPoll {
            ready_status: 200,
            ..
          }
        ));
      }
    }
  }

  #[test]
  fn select_backend_returns_llamacpp_for_any_gguf() {
    use crate::launch::flag_aliases::knob_specs;
    let b = select_backend(std::path::Path::new("/models/anything.gguf"));
    assert_eq!(b.id(), "llamacpp");
    assert_eq!(b.lifecycle(), Lifecycle::ProcessPerModel);
    // The selected backend exposes the full capability set (R6 data seam).
    for spec in knob_specs() {
      assert!(b.capabilities().supports(spec.field));
    }
  }

  #[test]
  fn lifecycle_labels_are_stable() {
    assert_eq!(Lifecycle::ProcessPerModel.label(), "process_per_model");
    assert_eq!(Lifecycle::ManagedMultiplexer.label(), "managed_multiplexer");
  }
}
