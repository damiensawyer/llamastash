//! Memory-budget estimators (weights + per-token KV) reused by U8
//! admission control.
//!
//! **Note:** this module no longer drives launch-time ctx sizing — that
//! is delegated to llama-server's `--fit` (with `--fit-ctx` as a floor),
//! and llamastash keeps budget *authority* through admission, not by
//! computing a ctx here. The original "fit mis-reports unified-memory
//! free space" claim was a May-2026 condition fixed underneath
//! llama.cpp by GPU-stack updates; the one remaining UMA weakness is
//! handled by the sysfs-backed admission reading (U8), not this module.
//! The estimator functions below survive because admission reuses the
//! same weights/KV math.
//!
//! Computation (as used by the admission estimator):
//! * Pick the budget pool. Any GPU off-load (`n_gpu_layers > 0`) → free
//!   VRAM from the daemon's host-metrics sampler. CPU-only run → RAM
//!   headroom. We treat "any GPU layers" as "all GPU layers"; partial
//!   off-load is rare and the built-in `--fit` still catches mistakes.
//! * Subtract the full model weight bytes (from the GGUF tensor table)
//!   and a fixed overhead for compute buffers + prompt cache headroom.
//! * Divide by `n_parallel` and by the per-token KV cache bytes
//!   computed from the GGUF attention geometry under the chosen cache
//!   dtype.
//! * Clamp to `[FLOOR_TOKENS, n_ctx_train]` and align down to 256.
//!
//! `None` is returned whenever any input is missing (no host-metrics
//! snapshot yet, GGUF lacks attention geometry, budget is too tight)
//! so the caller falls back to llama.cpp's own `--fit` rather than
//! guessing wrong.

use std::path::Path;

use crate::config::{KnobValueOpt, TypedKnobs};
use crate::daemon::host_metrics::{GpuFlavor, HostMetricsSnapshot};
use crate::gguf::header::{read_path, GgufHeader, HeaderReadOptions};
use crate::gguf::memory::{kv_bytes, weights_bytes, CacheType, EstimateOptions};

/// Lower bound the fit will produce when there is at least *some*
/// budget — matches `--fit-ctx`'s default floor so we never undershoot
/// llama.cpp's own minimum.
pub const FLOOR_TOKENS: u32 = 4096;

/// Alignment of the chosen ctx. llama.cpp aligns physical batches and
/// KV cache pages internally; rounding down to a 256-token multiple
/// keeps the argv tidy and avoids off-by-one fragmentation when
/// comparing against logs.
pub const ALIGN_TOKENS: u32 = 256;

/// Fixed overhead reserved on top of weights + KV cache: compute
/// buffer for the largest batch, llama.cpp's prompt-cache headroom,
/// and a safety margin for driver-side allocations the GGUF header
/// can't predict. 1.5 GiB is a deliberate over-estimate — undershooting
/// here is what made `--fit` ship a sub-optimal `n_ctx` in the first
/// place.
pub const OVERHEAD_BYTES: u64 = 1_536 * 1024 * 1024;

/// llama-server's auto-pick when `--parallel` is unset. Matches the
/// `n_parallel = 4` line that llama-server logs on startup so the
/// computation here mirrors what the child actually uses.
pub const DEFAULT_PARALLEL: u32 = 4;

/// Hard upper bound — the same ceiling `start_model`'s validator
/// applies to caller-supplied `ctx` values (`MAX_CTX_TOKENS`).
pub const HARD_CAP_TOKENS: u32 = 1_048_576;

/// Compute the auto-fit context length for `model_path` under `knobs`,
/// reading the latest host-metrics snapshot from `host`. Returns
/// `None` when any required input is missing — the caller should treat
/// that as "leave ctx unset and let llama.cpp `--fit` decide".
pub fn compute_ctx(
  model_path: &Path,
  knobs: &TypedKnobs,
  host: &HostMetricsSnapshot,
) -> Option<u32> {
  let header = read_path(model_path, HeaderReadOptions::default())
    .ok()?
    .header;
  compute_ctx_from_header(&header, knobs, host)
}

/// Same as [`compute_ctx`] but takes an already-parsed header — used
/// by tests and by callers that have the header on hand already.
pub fn compute_ctx_from_header(
  header: &GgufHeader,
  knobs: &TypedKnobs,
  host: &HostMetricsSnapshot,
) -> Option<u32> {
  let arch = header.string(&["general.architecture"])?.to_string();

  // An `Auto` knob reads as "no pinned value" here (`set_value()` →
  // `None`), so the estimator falls back to its own defaults exactly as
  // it did for an unset knob — `--fit` does the real placement.
  let n_parallel = knobs
    .parallel
    .set_value()
    .copied()
    .unwrap_or(DEFAULT_PARALLEL)
    .max(1);
  let n_gpu_layers = knobs.n_gpu_layers.set_value().copied();
  let cache_type_k = parse_cache_type(knobs.cache_type_k.set_value().map(String::as_str));
  let cache_type_v = parse_cache_type(knobs.cache_type_v.set_value().map(String::as_str));

  let n_ctx_train = header
    .u64(&[format!("{arch}.context_length")])
    .map(|v| v.min(HARD_CAP_TOKENS as u64) as u32)
    .unwrap_or(HARD_CAP_TOKENS);

  let budget = available_budget(host, n_gpu_layers)?;
  let weights = weights_bytes(header);

  // Per-token KV bytes for a single slot — divide the budget across
  // `n_parallel` slots downstream. Asking `kv_bytes` for `ctx_len = 1`
  // gives us exactly that.
  let probe_opts = EstimateOptions {
    ctx_len: 1,
    cache_type_k,
    cache_type_v,
    n_gpu_layers,
  };
  let kv_per_token = kv_bytes(header, Some(&arch), probe_opts);
  if kv_per_token == 0 {
    return None;
  }

  let available = budget
    .saturating_sub(weights)
    .saturating_sub(OVERHEAD_BYTES);
  if available == 0 {
    return None;
  }

  let per_slot_budget = available / n_parallel as u64;
  let raw_ctx = per_slot_budget / kv_per_token;
  let ctx = raw_ctx.min(n_ctx_train as u64).min(HARD_CAP_TOKENS as u64) as u32;

  if ctx < FLOOR_TOKENS {
    // Budget too tight to satisfy the floor. Returning `None` tells
    // the caller to leave `--fit` in charge rather than forcing a
    // too-small ctx that would just OOM differently.
    return None;
  }

  Some(align_down(ctx, ALIGN_TOKENS).max(FLOOR_TOKENS))
}

fn align_down(value: u32, align: u32) -> u32 {
  if align == 0 {
    return value;
  }
  value - (value % align)
}

/// Free byte budget for the chosen backend. Any GPU off-load
/// (`n_gpu_layers` > 0) uses VRAM; otherwise system RAM. Partial
/// off-load isn't modelled — the built-in `--fit` still catches
/// mistakes if a partial off-load setup picks too-large a ctx here.
fn available_budget(host: &HostMetricsSnapshot, n_gpu_layers: Option<u32>) -> Option<u64> {
  let wants_gpu = matches!(n_gpu_layers, Some(n) if n > 0);
  match host.flavor() {
    GpuFlavor::Unsampled => None,
    GpuFlavor::CpuOnly => Some(ram_free(host)),
    _ if !wants_gpu => Some(ram_free(host)),
    _ => {
      let total = host.gpu_mem_total_bytes?;
      let used = host.gpu_mem_used_bytes.unwrap_or(0);
      Some(total.saturating_sub(used))
    }
  }
}

fn ram_free(host: &HostMetricsSnapshot) -> u64 {
  host.ram_total_bytes.saturating_sub(host.ram_used_bytes)
}

/// Parse a `--cache-type-{k,v}` tag (`q8_0`, `f16`, …) into a
/// [`CacheType`], defaulting to `f16` when absent/unrecognised. Shared
/// with U8 admission so the KV projection uses the same dtype mapping.
pub fn parse_cache_type(raw: Option<&str>) -> CacheType {
  raw.and_then(CacheType::parse).unwrap_or_default()
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::config::KnobValue;
  use crate::daemon::host_metrics::HostMetricsSnapshot;
  use crate::gguf::header::{read_reader, HeaderReadOptions};
  use crate::gguf::test_fixtures::FixtureBuilder;
  use std::io::Cursor as IoCursor;

  // ~16 GiB at F16 (8.59e9 elements * 2 bytes). Enough that the
  // budget math is realistic against a 96 GiB iGPU pool.
  const WEIGHTS_ELEMS: u64 = 8_589_934_592;

  fn header(arch: &str) -> GgufHeader {
    let bytes = FixtureBuilder::new()
      .with_arch(arch)
      // n_layers=64, n_heads=64, n_kv_heads=8, head_dim=128.
      // Per-token KV (f16/f16): 2 * 64 * 8 * 128 * 2 = 262144 bytes.
      .with_block_count(64)
      .with_head_count(64)
      .with_head_count_kv(8)
      .with_embedding_length(64 * 128)
      .with_context_length(262_144)
      .with_tensor("blk.0.weight", &[WEIGHTS_ELEMS], 1)
      .build();
    read_reader(IoCursor::new(bytes), HeaderReadOptions::default())
      .unwrap()
      .header
  }

  fn amd_host(total_gib: u64, used_gib: u64) -> HostMetricsSnapshot {
    HostMetricsSnapshot {
      gpu_backend: HostMetricsSnapshot::BACKEND_AMD.into(),
      gpu_device_count: 1,
      gpu_mem_total_bytes: Some(total_gib * 1024 * 1024 * 1024),
      gpu_mem_used_bytes: Some(used_gib * 1024 * 1024 * 1024),
      ..HostMetricsSnapshot::default()
    }
  }

  fn knobs_all_gpu() -> TypedKnobs {
    TypedKnobs {
      n_gpu_layers: Some(KnobValue::Set(99)),
      parallel: Some(KnobValue::Set(1)),
      ..TypedKnobs::default()
    }
  }

  #[test]
  fn large_vram_picks_full_context() {
    // 96 GiB total - 0 used = 96 GiB free, 1 slot, ~16 GiB weights → plenty.
    let h = header("qwen3");
    let host = amd_host(96, 0);
    let ctx = compute_ctx_from_header(&h, &knobs_all_gpu(), &host).unwrap();
    assert!(ctx >= 200_000, "ctx={ctx}");
    assert!(ctx <= 262_144);
    assert_eq!(ctx % ALIGN_TOKENS, 0);
  }

  #[test]
  fn tight_vram_returns_none() {
    // 18 GiB total: ~16 GiB weights + 1.5 GiB overhead leaves ~0.5 GiB
    // for KV, which can't cover 4096 tokens at this geometry → None.
    let h = header("qwen3");
    let host = amd_host(18, 0);
    let res = compute_ctx_from_header(&h, &knobs_all_gpu(), &host);
    assert!(res.is_none(), "got ctx={res:?}");
  }

  #[test]
  fn n_parallel_4_quarters_the_ctx() {
    // Use a smaller VRAM pool so neither single nor 4-slot ctx clamps
    // to n_ctx_train; that way the parallel-divides-budget property is
    // observable.
    let h = header("qwen3");
    let host = amd_host(22, 0);
    let mut knobs = knobs_all_gpu();
    let one = compute_ctx_from_header(&h, &knobs, &host).unwrap();
    knobs.parallel = Some(KnobValue::Set(4));
    let four = compute_ctx_from_header(&h, &knobs, &host).unwrap();
    assert!(
      one > four * 3,
      "single-slot ctx should be ~4x the 4-slot ctx: one={one} four={four}"
    );
  }

  #[test]
  fn smaller_kv_dtype_expands_ctx() {
    // Constrained pool so the n_ctx_train ceiling doesn't mask the
    // dtype-driven difference. F16 (2 bpe) vs Q4_0 (0.5625 bpe).
    let h = header("qwen3");
    let host = amd_host(22, 0);
    let f16 = compute_ctx_from_header(&h, &knobs_all_gpu(), &host).unwrap();
    let knobs_q4 = TypedKnobs {
      n_gpu_layers: Some(KnobValue::Set(99)),
      parallel: Some(KnobValue::Set(1)),
      cache_type_k: Some(KnobValue::Set("q4_0".into())),
      cache_type_v: Some(KnobValue::Set("q4_0".into())),
      ..TypedKnobs::default()
    };
    let q4 = compute_ctx_from_header(&h, &knobs_q4, &host).unwrap();
    assert!(q4 > f16, "q4={q4} f16={f16}");
  }

  #[test]
  fn missing_snapshot_yields_none() {
    let h = header("qwen3");
    let host = HostMetricsSnapshot {
      gpu_backend: HostMetricsSnapshot::UNINITIALIZED_BACKEND.into(),
      ..HostMetricsSnapshot::default()
    };
    let res = compute_ctx_from_header(&h, &knobs_all_gpu(), &host);
    assert!(res.is_none());
  }

  #[test]
  fn unknown_arch_geometry_yields_none() {
    let bytes = FixtureBuilder::new()
      .with_arch("mystery")
      .with_tensor("some.weight", &[64], 1)
      .build();
    let h = read_reader(IoCursor::new(bytes), HeaderReadOptions::default())
      .unwrap()
      .header;
    let host = amd_host(96, 16);
    assert!(compute_ctx_from_header(&h, &knobs_all_gpu(), &host).is_none());
  }

  #[test]
  fn cpu_only_uses_ram_budget() {
    let h = header("qwen3");
    let host = HostMetricsSnapshot {
      gpu_backend: HostMetricsSnapshot::BACKEND_CPU_ONLY.into(),
      ram_total_bytes: 96 * 1024 * 1024 * 1024,
      ram_used_bytes: 16 * 1024 * 1024 * 1024,
      ..HostMetricsSnapshot::default()
    };
    let knobs = TypedKnobs {
      // CPU-only run keeps n_gpu_layers None — the helper falls back
      // to RAM headroom.
      parallel: Some(KnobValue::Set(1)),
      ..TypedKnobs::default()
    };
    let ctx = compute_ctx_from_header(&h, &knobs, &host).unwrap();
    assert!(ctx >= FLOOR_TOKENS);
  }
}
