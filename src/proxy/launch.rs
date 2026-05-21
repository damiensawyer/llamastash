//! Proxy-side launch helper.
//!
//! When a `/v1/...` request lands for a model that exists in the
//! catalog but has no Ready supervisor, [`auto_start`] drives the
//! launch in-process by calling
//! [`crate::ipc::methods::start_model_inner`] — the same composition
//! pipeline the IPC `start_model` handler uses, so the two paths
//! can't drift apart.
//!
//! The flow:
//!   1. Build a default [`StartParams`] from the resolved catalog
//!      row (just the path; mode defaults to Chat, no port
//!      preference, no caller knobs — `start_model_inner` then
//!      replays the same `last_params → arch_defaults → built-in`
//!      cascade the IPC handler does).
//!   2. Acquire single-flight rights via
//!      [`crate::proxy::coalesce::Coalesce::acquire`]. Leaders run
//!      `start_model_inner`; followers `.wait()` and rejoin via the
//!      supervisor snapshot.
//!   3. Poll [`crate::daemon::supervisor::ManagedModel::state`] at
//!      100 ms cadence until it reaches `Ready` (forward) or
//!      `Error{cause}` (fallback). No client-facing timeout — per
//!      the locked Key Decision "Hard supervisor Error only; wait
//!      indefinitely on Loading."
//!
//! Plan: docs/plans/2026-05-21-001-feat-proxy-router-plan.md (Unit 4).

use std::sync::Arc;
use std::time::Duration;

use crate::cli::resolve::CatalogRow;
use crate::daemon::supervisor::{ManagedModel, ManagedState};
use crate::gguf::identity::ModelId;
use crate::ipc::methods::{start_model_inner, StartParams};

use super::coalesce::AcquireOutcome;
use super::state::ProxyState;

/// Outcome of [`auto_start`]. The proxy's caller branches on this:
/// `Ready` forwards against `(model, port)`; `Failed` enters the
/// family-MRU fallback path.
pub(crate) enum LaunchOutcome {
  /// Supervisor reached `ManagedState::Ready`. The caller owns the
  /// `ManagedModel` for forwarding and the `port` for the upstream
  /// URL.
  Ready {
    #[allow(dead_code)]
    model: ManagedModel,
    port: u16,
    #[allow(dead_code)]
    model_id: ModelId,
  },
  /// Launch hit a terminal error before reaching Ready. `cause`
  /// surfaces in the 503 `launch_failed` JSON body when no fallback
  /// is available.
  Failed { cause: String },
}

/// Drive a launch (or wait on an in-flight one) and resolve to the
/// `ManagedModel` + port once Ready. Returns [`LaunchOutcome::Failed`]
/// if the supervisor reaches `Error{cause}` before Ready.
///
/// The proxy must hold `Arc<ProxyState>` for the duration so the
/// coalesce + supervisor handles stay alive across the await
/// points.
pub(crate) async fn auto_start(state: &Arc<ProxyState>, resolved: &CatalogRow) -> LaunchOutcome {
  // Compute the canonical ModelId from the resolved row. We read
  // the header here rather than trusting any in-process cache so
  // the single-flight key matches what `start_model_inner` will
  // observe at spawn time (it does the same read internally).
  let model_id = match canonical_id_for_row(resolved) {
    Ok(id) => id,
    Err(cause) => return LaunchOutcome::Failed { cause },
  };

  // Single-flight acquire. Leaders run the launch; followers park
  // until the leader signals completion, then re-snapshot the
  // supervisor map and proceed independently — happy followers
  // forward against the now-Ready model; the few that observed a
  // `Error{cause}` rejoin the fallback path.
  match state.coalesce.acquire(model_id.clone()).await {
    AcquireOutcome::Leader(leader) => {
      let outcome = drive_launch_as_leader(state, resolved).await;
      leader.finish().await;
      outcome
    }
    AcquireOutcome::Follower(follower) => {
      follower.wait().await;
      // Re-snapshot the supervisor map. The leader's launch may
      // have succeeded (this is the common case — single-flight
      // savings) or failed. Failed launches fall through to the
      // caller's fallback selector independently for this request,
      // per R155's per-request retry rule.
      match find_ready_supervisor(state, &model_id).await {
        Some((model, port)) => LaunchOutcome::Ready {
          model,
          port,
          model_id,
        },
        None => LaunchOutcome::Failed {
          cause: "launch failed in concurrent request".to_string(),
        },
      }
    }
  }
}

/// Run [`start_model_inner`], then poll `state()` at 100 ms until
/// the supervisor reaches `Ready` or `Error`. Pulled out so the
/// leader arm of [`auto_start`] reads top-to-bottom without nesting.
async fn drive_launch_as_leader(state: &Arc<ProxyState>, resolved: &CatalogRow) -> LaunchOutcome {
  let params = StartParams {
    model_path: std::path::PathBuf::from(&resolved.path),
    ..StartParams::default()
  };
  let started = match start_model_inner(&state.ctx, params).await {
    Ok(s) => s,
    Err(e) => {
      return LaunchOutcome::Failed {
        cause: format!("start_model_inner: {}", e.message),
      };
    }
  };

  // Poll the supervisor state machine. 100 ms cadence per the Key
  // Decision; no client-facing timeout — only `Error{cause}`
  // triggers fallback (Loading waits indefinitely).
  loop {
    match started.model.state().await {
      ManagedState::Ready => {
        return LaunchOutcome::Ready {
          model: started.model.clone(),
          port: started.port,
          model_id: started.model_id.clone(),
        };
      }
      ManagedState::Error { cause } => {
        return LaunchOutcome::Failed { cause };
      }
      ManagedState::Stopped => {
        // Process exited before reaching Ready and without the
        // probe stamping an Error{cause}. Treat this as a launch
        // failure with a generic cause so the fallback path
        // engages — surfacing the raw state would leak a wire
        // shape clients can't act on.
        return LaunchOutcome::Failed {
          cause: "supervisor exited before reaching Ready".to_string(),
        };
      }
      ManagedState::Launching | ManagedState::Loading | ManagedState::Stopping => {
        tokio::time::sleep(Duration::from_millis(100)).await;
      }
    }
  }
}

/// Walk the supervisor snapshot for a Ready entry whose canonical
/// path matches `id.path`. Used by the follower arm to confirm the
/// leader's launch reached Ready.
async fn find_ready_supervisor(
  state: &Arc<ProxyState>,
  id: &ModelId,
) -> Option<(ManagedModel, u16)> {
  let snap = state.supervisors.snapshot().await;
  for (_launch_id, model) in snap.into_iter() {
    if model.id().path != id.path {
      continue;
    }
    if matches!(model.state().await, ManagedState::Ready) {
      return Some((model.clone(), model.port()));
    }
  }
  None
}

/// Compute the canonical [`ModelId`] for a resolved [`CatalogRow`].
/// Reads the GGUF header on the daemon side rather than trusting
/// the catalog's pre-cached `model_id`; matches what
/// [`start_model_inner`] will observe internally so the single-flight
/// key is consistent end-to-end.
fn canonical_id_for_row(row: &CatalogRow) -> Result<ModelId, String> {
  let path = std::path::Path::new(&row.path);
  let header =
    crate::gguf::header::read_path(path, crate::gguf::header::HeaderReadOptions::default())
      .map_err(|e| format!("could not read GGUF header at {}: {e}", row.path))?;
  Ok(crate::gguf::identity::compute(path, &header.raw))
}
