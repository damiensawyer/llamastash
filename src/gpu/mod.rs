//! Best-effort GPU detection at daemon start (R44).
//!
//! v1 baseline strategy: shell out to vendor tools and parse their
//! human/JSON output rather than linking native SDKs (NVML, ROCm,
//! Metal). This keeps the build portable across CUDA / ROCm /
//! Apple Silicon machines without conditional native deps. Future
//! follow-up: replace the shell-out with `nvml-wrapper` on Linux
//! for accurate per-PID VRAM attribution.
//!
//! Detection order:
//! 1. NVIDIA via `nvidia-smi --query-gpu=...` (Linux + Windows) — wins
//!    when available because it surfaces live util%/temperature that
//!    DXGI can't.
//! 2. AMD via `rocm-smi --showmeminfo vram --json` (Linux). Windows
//!    AMD doesn't ship `rocm-smi.exe`, so the DXGI step below covers
//!    it.
//! 3. **Windows-only:** DXGI via `IDXGIFactory1::EnumAdapters1` —
//!    static adapter name + dedicated VRAM + shared system memory
//!    for AMD / Intel / and the rare NVIDIA-without-nvidia-smi.exe
//!    stripped-install case. No live metrics (DXGI doesn't expose
//!    them); host pane renders `—` for util/temp on this path.
//! 4. Apple Silicon Metal via `system_profiler SPDisplaysDataType
//!    -json` (macOS).
//! 5. Vulkan fallback (`vulkaninfo --summary`) — Linux Vulkan-only
//!    AMD or Intel Arc machines without rocm-smi. Reports adapter
//!    names only; surfaces under `Unknown`.
//! 6. Final fallback: `CpuOnly` — supervisor still runs.

pub mod amd;
#[cfg(windows)]
pub mod dxgi;
pub mod metal;
pub mod nvidia;
pub mod vulkan;

use std::process::{Command, Output};
use std::time::Duration;

use serde::Serialize;

/// Wall-clock budget for a single vendor probe. A wedged GPU driver
/// (nvidia-smi hang, ROCm reset, locked Vulkan loader) would otherwise
/// pin the blocking pool thread indefinitely. Five seconds is well
/// above any normal vendor-tool invocation on healthy hardware.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Run an external probe with a wall-clock deadline. On expiry the
/// child is killed; the call returns `None` so the probe chain can
/// fall through to the next backend instead of stalling the daemon.
///
/// Delegates to [`crate::util::process::run_with_drain_and_timeout`]
/// so the spawn-poll-drain pattern is shared with smoke and brew.
pub(crate) fn run_with_timeout(cmd: Command) -> Option<Output> {
  let program = format!("{:?}", cmd.get_program());
  match crate::util::process::run_with_drain_and_timeout(cmd, PROBE_TIMEOUT) {
    Ok(out) => Some(out),
    Err(crate::util::process::RunError::Timeout { after }) => {
      log::warn!("gpu probe `{program}` exceeded {after:?}; killed and falling through");
      None
    }
    Err(_) => None,
  }
}

/// What detection found. Always a complete snapshot — no
/// "partial" / "unknown" middle ground — so the IPC handler can
/// serialise it directly into `status`.
///
/// Single-backend hits return the corresponding variant; when two or
/// more backends each find at least one device the `Multi` variant
/// carries all of them (each tagged with its backend) so the host
/// stats pane can render per-GPU rows instead of hiding half the
/// hardware.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "backend", rename_all = "snake_case")]
pub enum GpuInfo {
  /// No GPU detected (or detection failed). The daemon still runs;
  /// `llama-server` falls back to CPU inference.
  CpuOnly,
  /// NVIDIA card(s) found. Multi-GPU machines surface as a list of
  /// devices.
  Nvidia { devices: Vec<GpuDevice> },
  /// AMD card(s) found.
  Amd { devices: Vec<GpuDevice> },
  /// Apple Silicon — unified-memory GPU. Reports the system memory
  /// available to the GPU since Metal doesn't separate VRAM.
  AppleMetal { total_memory_bytes: u64 },
  /// `vulkaninfo` found a device but neither NVIDIA nor ROCm probes
  /// succeeded, so the vendor is unknown. The supervisor still hints
  /// that the user can attempt `-ngl > 0`; the host pane renders
  /// `backend  unknown` rather than mislabelling the card.
  Unknown { devices: Vec<GpuDevice> },
  /// Multiple backends each found one or more GPUs. Carries a
  /// per-device `backend` tag so callers can group / label them
  /// independently.
  Multi { devices: Vec<GpuDevice> },
}

/// One discrete GPU device (NVIDIA / AMD path).
///
/// `utilization_pct` and `temperature_c` are best-effort: the per-tick
/// host-metrics sampler reads them from vendor tools that may or may
/// not expose them on a given platform / driver version. When a probe
/// can't surface them they stay `None`; the host stats pane renders
/// `—` in place of a numeric reading rather than dropping the row.
///
/// `backend` tags which probe produced this device ("nvidia", "amd",
/// "apple_metal", or "unknown"). Used when combining multi-backend
/// snapshots into a `GpuInfo::Multi`.
///
/// Note: this struct intentionally does not derive `Eq` because the
/// `f32` fields don't satisfy `Eq` (NaN-not-equal-to-itself). The
/// `PartialEq` derive is sufficient for the only equality use case
/// today — round-tripping in tests. Downstream consumers needing a
/// hashable / `Eq`-bound view should compare a projection (e.g. the
/// `name` + `total_memory_bytes` fields) rather than the whole struct.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct GpuDevice {
  pub name: String,
  pub backend: String,
  pub total_memory_bytes: u64,
  pub used_memory_bytes: u64,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub utilization_pct: Option<f32>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub temperature_c: Option<f32>,
  /// Portion of `total_memory_bytes` that lives in the system RAM
  /// pool (e.g. AMD GTT on UMA APUs like Strix Halo). When `Some`,
  /// the host pane subtracts this from the RAM gauge so the same
  /// bytes aren't counted twice (once as VRAM, once as system RAM).
  /// `None` on discrete cards and any backend without a UMA mode.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub uma_shared_total_bytes: Option<u64>,
  /// Currently-allocated portion of `uma_shared_total_bytes`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub uma_shared_used_bytes: Option<u64>,
}

impl GpuInfo {
  pub fn label(&self) -> &'static str {
    match self {
      Self::CpuOnly => "cpu_only",
      Self::Nvidia { .. } => "nvidia",
      Self::Amd { .. } => "amd",
      Self::AppleMetal { .. } => "apple_metal",
      Self::Unknown { .. } => "unknown",
      Self::Multi { .. } => "multi",
    }
  }

  /// Return the backends present in this snapshot. Used by the host
  /// stats pane to build a combined backend label (e.g. `"NVML · 1 GPU + ROCm · 1 GPU"`).
  pub fn is_gpu(&self) -> bool {
    !matches!(self, Self::CpuOnly)
  }

  /// Single source of truth for "is this backend unified memory?" —
  /// the GPU shares one physical pool with the CPU rather than owning
  /// dedicated VRAM. Both the init banner and the TUI host pane render
  /// from this so the two never disagree (the `*`/"unified" marker).
  ///
  /// - Apple Silicon (Metal) is unified by construction.
  /// - AMD / Nvidia / Unknown are unified when a device carries a
  ///   `uma_shared_total_bytes` portion — set by `rocm-smi`'s GTT pool
  ///   on Linux APUs and by the D3D12 `UMA` architecture flag on
  ///   Windows. Discrete cards never populate it.
  /// - CpuOnly has no GPU memory at all.
  pub fn is_unified(&self) -> bool {
    match self {
      Self::AppleMetal { .. } => true,
      Self::Multi { devices } => devices.iter().any(|d| d.uma_shared_total_bytes.is_some()),
      Self::Nvidia { devices } | Self::Amd { devices } | Self::Unknown { devices } => {
        devices.iter().any(|d| d.uma_shared_total_bytes.is_some())
      }
      Self::CpuOnly => false,
    }
  }

  /// Return the set of backend labels present in this snapshot. Used
  /// by the host stats pane to build a combined backend label
  /// (e.g. `"NVML · 1 GPU + ROCm · 1 GPU"`).
  pub fn backends(&self) -> Vec<String> {
    match self {
      Self::CpuOnly => vec![],
      Self::Multi { devices } => {
        let mut seen = std::collections::BTreeSet::new();
        for d in devices {
          seen.insert(d.backend.clone());
        }
        seen.into_iter().collect()
      }
      Self::Nvidia { .. } => vec!["nvidia".into()],
      Self::Amd { .. } => vec!["amd".into()],
      Self::AppleMetal { .. } => vec!["apple_metal".into()],
      Self::Unknown { .. } => vec!["unknown".into()],
    }
  }
}

/// Run the full detection chain. Best-effort — every probe failure
/// falls through to the next backend. Unlike the v1 single-hit probe,
/// this collects from **all** backends and returns a `Multi` snapshot
/// when two or more backends each find at least one device. A single-
/// backend hit returns that backend's variant for backward compat.
///
/// Suitable for daemon startup and periodic hotplug-detection
/// passes; the per-tick host-metrics refresh uses [`refresh_active`]
/// to avoid spawning every vendor tool every second.
pub fn probe() -> GpuInfo {
  let mut nvidia_devices: Vec<GpuDevice> = Vec::new();
  let mut amd_devices: Vec<GpuDevice> = Vec::new();
  let mut metal_devices: Vec<GpuDevice> = Vec::new();
  let mut unknown_devices: Vec<GpuDevice> = Vec::new();

  // NVIDIA probe
  if let Some(devs) = nvidia::probe_devices() {
    nvidia_devices = devs;
  }
  // AMD probe
  if let Some(devs) = amd::probe_devices() {
    amd_devices = devs;
  }
  // Windows-only: DXGI fills the AMD / Intel slot that `rocm-smi`
  // doesn't reach. Also catches NVIDIA on stripped Windows installs
  // where `nvidia-smi.exe` isn't on PATH. Static memory totals only —
  // no live util/temp.
  #[cfg(windows)]
  {
    if let Some(devs) = dxgi::probe_devices() {
      amd_devices.extend(devs.clone());
    }
  }
  // Apple Silicon probe
  if let Some(devs) = metal::probe_devices() {
    metal_devices = devs;
  }
  // Vulkan fallback
  if let Some(devs) = vulkan::probe_devices() {
    unknown_devices = devs;
  }

  // Count total devices across all backends
  let total =
    nvidia_devices.len() + amd_devices.len() + metal_devices.len() + unknown_devices.len();

  if total == 0 {
    return GpuInfo::CpuOnly;
  }

  // Single-device hits return the native variant for backward compat
  if total == 1 && nvidia_devices.is_empty() && amd_devices.is_empty() && unknown_devices.is_empty()
  {
    // Only Metal — return AppleMetal for the unified-memory path
    let dev = &metal_devices[0];
    return GpuInfo::AppleMetal {
      total_memory_bytes: dev.total_memory_bytes,
    };
  }
  if total == 1 && amd_devices.is_empty() && metal_devices.is_empty() && unknown_devices.is_empty()
  {
    return GpuInfo::Nvidia {
      devices: nvidia_devices,
    };
  }
  if total == 1
    && nvidia_devices.is_empty()
    && metal_devices.is_empty()
    && unknown_devices.is_empty()
  {
    return GpuInfo::Amd {
      devices: amd_devices,
    };
  }
  if total == 1 && nvidia_devices.is_empty() && amd_devices.is_empty() && metal_devices.is_empty() {
    return GpuInfo::Unknown {
      devices: unknown_devices,
    };
  }

  // Two or more backends — combine all devices with backend tags.
  // Vulkan devices are added last (lowest priority) and only if
  // they weren't already found by a more specific probe (CUDA/ROCm).
  let mut all_devices: Vec<GpuDevice> = Vec::new();
  // Collect names from non-Vulkan probes for dedup
  let non_vulkan_names: Vec<String> = {
    let mut names = Vec::new();
    for d in &nvidia_devices {
      names.push(d.name.clone());
    }
    for d in &amd_devices {
      names.push(d.name.clone());
    }
    for d in &metal_devices {
      names.push(d.name.clone());
    }
    names
  };
  let any_seen = |name: &str| {
    non_vulkan_names.iter().any(|n| {
      let n_lc = n.to_lowercase();
      let name_lc = name.to_lowercase();
      n_lc == name_lc || name_lc.contains(&n_lc) || n_lc.contains(&name_lc)
    })
  };
  for d in nvidia_devices {
    all_devices.push(d);
  }
  for d in amd_devices {
    all_devices.push(d);
  }
  for d in metal_devices {
    all_devices.push(d);
  }
  // Vulkan devices only if not already seen
  for d in unknown_devices {
    let was_seen = any_seen(&d.name);
    if !was_seen {
      all_devices.push(d);
    }
  }
  GpuInfo::Multi {
    devices: all_devices,
  }
}

/// Refresh the already-detected backends by calling only their vendor
/// probes. Returns a new `GpuInfo` when at least one backend changed
/// this tick, `None` when nothing changed.
///
/// For single-backend hits the path is trivial (one vendor tool per
/// tick). For `Multi` we refresh every backend that previously had
/// devices so we catch driver rebinds, hotplugged cards, and late
/// driver loads.
///
/// This is the per-tick fast path used by the host-metrics sampler.
/// CPU-only / Vulkan / Metal hosts skip per-tick spawns entirely
/// (the periodic full re-probe in the sampler still catches hotplug /
/// late driver loads).
pub fn refresh_active(prev: &GpuInfo) -> Option<GpuInfo> {
  match prev {
    GpuInfo::CpuOnly | GpuInfo::AppleMetal { .. } | GpuInfo::Unknown { .. } => None,
    GpuInfo::Nvidia { .. } => nvidia::probe_devices().map(|d| GpuInfo::Nvidia { devices: d }),
    #[cfg(unix)]
    GpuInfo::Amd { .. } => amd::probe_devices().map(|d| GpuInfo::Amd { devices: d }),
    #[cfg(windows)]
    GpuInfo::Amd { .. } => None,
    GpuInfo::Multi { devices } => {
      // Derive per-backend lists from the backend tags.
      let prev_nvidia: Vec<GpuDevice> = devices
        .iter()
        .filter(|d| d.backend == "nvidia")
        .cloned()
        .collect();
      let prev_amd: Vec<GpuDevice> = devices
        .iter()
        .filter(|d| d.backend == "amd")
        .cloned()
        .collect();
      let prev_metal: Vec<GpuDevice> = devices
        .iter()
        .filter(|d| d.backend == "apple_metal")
        .cloned()
        .collect();
      let prev_unknown: Vec<GpuDevice> = devices
        .iter()
        .filter(|d| d.backend == "unknown")
        .cloned()
        .collect();

      let mut changed = false;
      let mut next_nvidia = prev_nvidia.clone();
      let mut next_amd = prev_amd.clone();
      let next_metal = prev_metal.clone();
      let next_unknown = prev_unknown.clone();
      if !prev_nvidia.is_empty() {
        if let Some(devs) = nvidia::probe_devices() {
          if !devices_match(&prev_nvidia, &devs) {
            next_nvidia = devs;
            changed = true;
          }
        }
      }
      if !prev_amd.is_empty() {
        if let Some(devs) = amd::probe_devices() {
          if !devices_match(&prev_amd, &devs) {
            next_amd = devs;
            changed = true;
          }
        }
      }
      // Metal and Vulkan data are static — no per-tick refresh needed.
      if changed {
        let mut all = Vec::new();
        all.extend(next_nvidia);
        all.extend(next_amd);
        all.extend(next_metal);
        all.extend(next_unknown);
        Some(GpuInfo::Multi { devices: all })
      } else {
        None
      }
    }
  }
}

/// Compare two device lists by name + total_memory_bytes.
/// We can't use `==` because `GpuDevice` intentionally doesn't
/// derive `Eq` (NaN-f32 fields). This is sufficient for detecting
/// changes in the active backend.
fn devices_match(a: &[GpuDevice], b: &[GpuDevice]) -> bool {
  if a.len() != b.len() {
    return false;
  }
  for (da, db) in a.iter().zip(b.iter()) {
    if da.name != db.name {
      return false;
    }
    if da.total_memory_bytes != db.total_memory_bytes {
      return false;
    }
  }
  true
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn cpu_only_is_not_gpu() {
    assert!(!GpuInfo::CpuOnly.is_gpu());
    assert_eq!(GpuInfo::CpuOnly.label(), "cpu_only");
  }

  #[test]
  fn nvidia_is_gpu() {
    let info = GpuInfo::Nvidia {
      devices: vec![GpuDevice {
        name: "RTX 4090".into(),
        total_memory_bytes: 24 * 1024 * 1024 * 1024,
        used_memory_bytes: 0,
        utilization_pct: None,
        temperature_c: None,
        ..Default::default()
      }],
    };
    assert!(info.is_gpu());
    assert_eq!(info.label(), "nvidia");
  }

  #[test]
  fn json_carries_tag_field() {
    let v = GpuInfo::AppleMetal {
      total_memory_bytes: 64 * 1024 * 1024 * 1024,
    };
    let s = serde_json::to_string(&v).unwrap();
    assert!(s.contains("\"backend\":\"apple_metal\""));
    assert!(s.contains("\"total_memory_bytes\":"));
  }

  #[test]
  fn gpu_device_omits_optional_fields_when_absent() {
    let dev = GpuDevice {
      name: "RTX 4090".into(),
      total_memory_bytes: 24 * 1024 * 1024 * 1024,
      used_memory_bytes: 0,
      utilization_pct: None,
      temperature_c: None,
      ..Default::default()
    };
    let s = serde_json::to_string(&dev).unwrap();
    assert!(!s.contains("utilization_pct"));
    assert!(!s.contains("temperature_c"));
  }

  #[test]
  fn gpu_device_emits_optional_fields_when_present() {
    let dev = GpuDevice {
      name: "RTX 4090".into(),
      total_memory_bytes: 24 * 1024 * 1024 * 1024,
      used_memory_bytes: 12 * 1024 * 1024 * 1024,
      utilization_pct: Some(84.0),
      temperature_c: Some(68.0),
      ..Default::default()
    };
    let s = serde_json::to_string(&dev).unwrap();
    assert!(s.contains("\"utilization_pct\":84"));
    assert!(s.contains("\"temperature_c\":68"));
  }
}
