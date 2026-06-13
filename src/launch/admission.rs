//! Pre-spawn memory admission control + in-memory reservation ledger (R4).
//!
//! llamastash delegates *placement* to llama-server's `--fit` but keeps
//! *budget authority*: before spawning a child it projects the launch's
//! demand floor against the sampled, post-headroom free memory minus the
//! bytes already reserved by in-flight launches. If the demand does not
//! fit, the launch is refused **before** spawn (cheap, deterministic) so
//! two concurrent oversized models can never double-book the same free
//! reading and OOM the box — the failure `--fit` alone can't prevent on
//! UMA, where its own free reading conflates the GTT pool with system
//! RAM.
//!
//! Design (kept deliberately simple — see plan scope amendment):
//! - **One combined budget.** UMA / Apple hosts budget the single
//!   physical pool (≈ system RAM); discrete hosts sum VRAM + system RAM.
//!   We compare combined demand against combined free rather than
//!   modelling a per-pool GPU/RAM split — conservative and adequate as a
//!   safety net.
//! - **Reservation = full demand**, held from admit until the child
//!   settles (Ready / Error / Stopped). While a child is Loading the
//!   sampler also sees its growing allocation, so the budget is counted
//!   slightly conservatively during that window — it errs toward
//!   refusing a second concurrent launch, never toward OOM.
//! - **Best-effort.** When there is no host-metrics sample yet
//!   (`unsampled`, or no sampler wired as in many tests) admission is
//!   skipped and the launch proceeds — we never block on missing data.
//! - **Never refuse on missing geometry.** A model whose GGUF lacks the
//!   attention fields contributes only its known weight bytes to demand.

use std::sync::Mutex;

use crate::config::{KnobValueOpt, TypedKnobs};
use crate::daemon::host_metrics::HostMetricsSnapshot;
use crate::gguf::header::GgufHeader;
use crate::gguf::memory::{kv_bytes, weights_bytes, EstimateOptions};
use crate::launch::ctx_fit::parse_cache_type;
use crate::launch::headroom::{admissible_bytes, overhead_band_bytes, PoolKind};

/// One in-flight launch's hold on the budget, keyed by `launch_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Reservation {
  launch_id: u64,
  bytes: u64,
}

/// In-memory reservation ledger. Shared across every launch entry point
/// (CLI `start`, TUI, proxy auto-start) via the daemon's
/// `MethodContext`, so check-and-reserve is atomic against concurrent
/// launches. Never persisted — restart safety comes from conservative
/// re-sampling, not from a durable ledger.
#[derive(Debug, Default)]
pub struct Ledger {
  inner: Mutex<Vec<Reservation>>,
}

/// Why a launch was refused, with the numbers needed to explain it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refusal {
  /// Projected demand floor (weights + KV + overhead band).
  pub demand_bytes: u64,
  /// Post-headroom free across the budget pool(s), before reservations.
  pub effective_free_bytes: u64,
  /// Bytes already reserved by other in-flight launches.
  pub reserved_bytes: u64,
}

impl Refusal {
  /// Free bytes actually available to this launch (effective − reserved).
  pub fn available_bytes(&self) -> u64 {
    self
      .effective_free_bytes
      .saturating_sub(self.reserved_bytes)
  }
}

impl Ledger {
  /// Atomically check `demand_bytes` against `effective_free_bytes` minus
  /// the bytes already reserved, and on success record the reservation.
  /// One lock spans the read-and-reserve so two concurrent leaders cannot
  /// both pass against the same free reading.
  pub fn try_admit(
    &self,
    launch_id: u64,
    demand_bytes: u64,
    effective_free_bytes: u64,
  ) -> Result<(), Refusal> {
    let mut held = self.inner.lock().expect("admission ledger poisoned");
    let reserved_bytes: u64 = held.iter().map(|r| r.bytes).sum();
    if demand_bytes > effective_free_bytes.saturating_sub(reserved_bytes) {
      return Err(Refusal {
        demand_bytes,
        effective_free_bytes,
        reserved_bytes,
      });
    }
    held.push(Reservation {
      launch_id,
      bytes: demand_bytes,
    });
    Ok(())
  }

  /// Drop the reservation for `launch_id` (on Ready / Error / Stopped, or
  /// when a refused launch releases its port). Idempotent.
  pub fn release(&self, launch_id: u64) {
    self
      .inner
      .lock()
      .expect("admission ledger poisoned")
      .retain(|r| r.launch_id != launch_id);
  }

  /// Total reserved bytes — for diagnostics and tests.
  pub fn reserved_bytes(&self) -> u64 {
    self
      .inner
      .lock()
      .expect("admission ledger poisoned")
      .iter()
      .map(|r| r.bytes)
      .sum()
  }
}

/// Headroom kind for the host's budget pool.
fn pool_kind(snap: &HostMetricsSnapshot) -> PoolKind {
  if snap.gpu_backend == HostMetricsSnapshot::BACKEND_APPLE_METAL {
    PoolKind::AppleUnified
  } else if snap.unified {
    PoolKind::IntegratedUma
  } else if snap.gpu_mem_total_bytes.is_some() {
    PoolKind::DiscreteVram
  } else {
    PoolKind::SystemRam
  }
}

/// `true` once the daemon has a real host-metrics sample (not the
/// pre-first-tick `unsampled` placeholder). Admission only engages when
/// this holds.
pub fn is_sampled(snap: &HostMetricsSnapshot) -> bool {
  snap.gpu_backend != HostMetricsSnapshot::UNINITIALIZED_BACKEND
}

/// Post-headroom free bytes across the budget pool(s). UMA / Apple hosts
/// budget the single physical pool (≈ system RAM); discrete hosts sum
/// post-headroom VRAM free + post-headroom system-RAM free.
pub fn effective_free_bytes(snap: &HostMetricsSnapshot) -> u64 {
  let ram_free = snap.ram_total_bytes.saturating_sub(snap.ram_used_bytes);
  let unified = snap.unified || snap.gpu_backend == HostMetricsSnapshot::BACKEND_APPLE_METAL;
  if unified {
    admissible_bytes(ram_free, pool_kind(snap))
  } else if let (Some(total), Some(used)) = (snap.gpu_mem_total_bytes, snap.gpu_mem_used_bytes) {
    let vram_free = total.saturating_sub(used);
    admissible_bytes(vram_free, PoolKind::DiscreteVram)
      + admissible_bytes(ram_free, PoolKind::SystemRam)
  } else {
    admissible_bytes(ram_free, PoolKind::SystemRam)
  }
}

/// Demand floor for a launch: model weights + KV cache at the effective
/// context window + the backend's fixed overhead band. `n_gpu_layers`
/// from the knobs bounds the KV/offload estimate; an `Auto`/unset value
/// is treated as full offload by the estimator. Missing attention
/// geometry yields a KV of 0, so demand degrades to weights + band
/// rather than refusing on missing data.
pub fn project_demand(
  header: &GgufHeader,
  arch: Option<&str>,
  knobs: &TypedKnobs,
  effective_ctx: u32,
  backend: &str,
) -> u64 {
  let opts = EstimateOptions {
    ctx_len: effective_ctx as u64,
    cache_type_k: parse_cache_type(knobs.cache_type_k.set_value().map(String::as_str)),
    cache_type_v: parse_cache_type(knobs.cache_type_v.set_value().map(String::as_str)),
    n_gpu_layers: knobs.n_gpu_layers.set_value().copied(),
  };
  weights_bytes(header)
    .saturating_add(kv_bytes(header, arch, opts))
    .saturating_add(overhead_band_bytes(backend))
}

#[cfg(test)]
mod tests {
  use super::*;

  const GIB: u64 = 1024 * 1024 * 1024;

  #[test]
  fn admits_when_demand_fits_and_records_reservation() {
    let ledger = Ledger::default();
    assert!(ledger.try_admit(1, 10 * GIB, 60 * GIB).is_ok());
    assert_eq!(ledger.reserved_bytes(), 10 * GIB);
  }

  #[test]
  fn refuses_when_demand_exceeds_free_minus_reservations() {
    let ledger = Ledger::default();
    // First model reserves 44 GiB of a 60 GiB pool.
    ledger
      .try_admit(1, 44 * GIB, 60 * GIB)
      .expect("first admits");
    // Second model wants 37 GiB; only 16 GiB remains → refused, never
    // double-booked against the same free reading.
    let refusal = ledger
      .try_admit(2, 37 * GIB, 60 * GIB)
      .expect_err("second must be refused");
    assert_eq!(refusal.reserved_bytes, 44 * GIB);
    assert_eq!(refusal.available_bytes(), 16 * GIB);
    assert_eq!(
      ledger.reserved_bytes(),
      44 * GIB,
      "refusal reserves nothing"
    );
  }

  #[test]
  fn release_frees_the_pool_for_a_retry() {
    let ledger = Ledger::default();
    ledger
      .try_admit(1, 44 * GIB, 60 * GIB)
      .expect("first admits");
    ledger
      .try_admit(2, 37 * GIB, 60 * GIB)
      .expect_err("refused while first holds");
    ledger.release(1);
    assert_eq!(ledger.reserved_bytes(), 0);
    ledger
      .try_admit(2, 37 * GIB, 60 * GIB)
      .expect("admits once the pool frees");
  }

  #[test]
  fn two_fitting_leaders_both_admit_and_sum() {
    let ledger = Ledger::default();
    ledger.try_admit(1, 20 * GIB, 60 * GIB).expect("first");
    ledger.try_admit(2, 30 * GIB, 60 * GIB).expect("second");
    assert_eq!(ledger.reserved_bytes(), 50 * GIB);
  }

  #[test]
  fn release_is_idempotent_and_targets_one_launch() {
    let ledger = Ledger::default();
    ledger.try_admit(1, 10 * GIB, 60 * GIB).unwrap();
    ledger.try_admit(2, 10 * GIB, 60 * GIB).unwrap();
    ledger.release(1);
    ledger.release(1); // no-op second time
    assert_eq!(ledger.reserved_bytes(), 10 * GIB);
  }

  fn snap(backend: &str, unified: bool, ram_total: u64, ram_used: u64) -> HostMetricsSnapshot {
    HostMetricsSnapshot {
      gpu_backend: backend.to_string(),
      unified,
      ram_total_bytes: ram_total,
      ram_used_bytes: ram_used,
      ..HostMetricsSnapshot::default()
    }
  }

  #[test]
  fn uma_budget_is_the_single_ram_pool() {
    let s = snap(HostMetricsSnapshot::BACKEND_AMD, true, 128 * GIB, 28 * GIB);
    // IntegratedUma uses 1.0 fraction → full free RAM.
    assert_eq!(effective_free_bytes(&s), 100 * GIB);
  }

  #[test]
  fn apple_budget_applies_075_headroom() {
    let s = snap(HostMetricsSnapshot::BACKEND_APPLE_METAL, true, 64 * GIB, 0);
    assert_eq!(effective_free_bytes(&s), 48 * GIB);
  }

  #[test]
  fn discrete_budget_sums_vram_and_ram_free() {
    let mut s = snap("nvidia", false, 128 * GIB, 64 * GIB);
    s.gpu_mem_total_bytes = Some(24 * GIB);
    s.gpu_mem_used_bytes = Some(8 * GIB);
    // 16 GiB VRAM free + 64 GiB RAM free, both at 1.0 fraction.
    assert_eq!(effective_free_bytes(&s), 80 * GIB);
  }

  #[test]
  fn unsampled_snapshot_is_not_sampled() {
    let s = snap(HostMetricsSnapshot::UNINITIALIZED_BACKEND, false, 0, 0);
    assert!(!is_sampled(&s));
    let s2 = snap(HostMetricsSnapshot::BACKEND_AMD, true, GIB, 0);
    assert!(is_sampled(&s2));
  }
}
