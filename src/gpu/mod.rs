//! Best-effort GPU detection at daemon start (R44).
//!
//! v1 baseline strategy: shell out to vendor tools and parse their
//! human/JSON output rather than linking native SDKs (NVML, ROCm,
//! Metal). This keeps the build portable across CUDA / ROCm /
//! Apple Silicon machines without conditional native deps. Future
//! follow-up: replace the shell-out with `nvml-wrapper` on Linux
//! for accurate per-PID VRAM attribution.
//!
//! Detection order, per the plan:
//! 1. NVIDIA via `nvidia-smi --query-gpu=...` (Linux + Windows).
//! 2. AMD via `rocm-smi --showmeminfo vram --json` (Linux).
//! 3. Apple Silicon Metal via `system_profiler SPDisplaysDataType
//!    -json` (macOS).
//! 4. Fallback: `CpuOnly` — the supervisor still runs, just without
//!    a GPU memory line in `status`.

pub mod amd;
pub mod metal;
pub mod nvidia;
pub mod vulkan;

use serde::Serialize;

/// What detection found. Always a complete snapshot — no
/// "partial" / "unknown" middle ground — so the IPC handler can
/// serialise it directly into `status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
}

/// One discrete GPU device (NVIDIA / AMD path).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GpuDevice {
  pub name: String,
  pub total_memory_bytes: u64,
  pub used_memory_bytes: u64,
}

impl GpuInfo {
  pub fn label(&self) -> &'static str {
    match self {
      Self::CpuOnly => "cpu_only",
      Self::Nvidia { .. } => "nvidia",
      Self::Amd { .. } => "amd",
      Self::AppleMetal { .. } => "apple_metal",
    }
  }

  pub fn is_gpu(&self) -> bool {
    !matches!(self, Self::CpuOnly)
  }
}

/// Run the full detection chain. Best-effort — every probe failure
/// just falls through to the next backend, then to `CpuOnly`.
/// Suitable for daemon startup; not called per-launch.
pub fn probe() -> GpuInfo {
  if let Some(info) = nvidia::probe() {
    return info;
  }
  if let Some(info) = amd::probe() {
    return info;
  }
  if let Some(info) = metal::probe() {
    return info;
  }
  // Vulkan check is a last-resort "is *anything* there?" signal —
  // it can't give us memory numbers, but the supervisor uses it to
  // hint that the user can probably set `-ngl > 0` even though we
  // don't know how much VRAM they have. Returns CpuOnly when even
  // Vulkan can't find a device.
  vulkan::probe().unwrap_or(GpuInfo::CpuOnly)
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
}
