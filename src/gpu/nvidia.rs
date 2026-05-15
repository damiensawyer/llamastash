//! NVIDIA GPU probe via `nvidia-smi`.
//!
//! Querying CSV output keeps the parser tiny and stable across
//! driver versions: `nvidia-smi --query-gpu=name,memory.total,
//! memory.used --format=csv,noheader,nounits` emits one line per
//! GPU with comma-separated MiB values.

use std::process::Command;

use super::{GpuDevice, GpuInfo};

/// Run `nvidia-smi`. Returns `None` if the binary isn't on `$PATH`
/// or its exit status is non-zero (no NVIDIA driver loaded).
pub fn probe() -> Option<GpuInfo> {
  let output = Command::new("nvidia-smi")
    .args([
      "--query-gpu=name,memory.total,memory.used",
      "--format=csv,noheader,nounits",
    ])
    .output()
    .ok()?;
  if !output.status.success() {
    return None;
  }
  let stdout = String::from_utf8(output.stdout).ok()?;
  let devices = parse(&stdout);
  if devices.is_empty() {
    return None;
  }
  Some(GpuInfo::Nvidia { devices })
}

/// Parse the `--format=csv,noheader,nounits` output. Exposed so unit
/// tests can pin the format without spawning a subprocess.
pub(crate) fn parse(stdout: &str) -> Vec<GpuDevice> {
  let mut out = Vec::new();
  for line in stdout.lines() {
    let trimmed = line.trim();
    if trimmed.is_empty() {
      continue;
    }
    let parts: Vec<&str> = trimmed.split(',').map(str::trim).collect();
    if parts.len() < 3 {
      continue;
    }
    let name = parts[0].to_string();
    let total_mib: u64 = match parts[1].parse() {
      Ok(v) => v,
      Err(_) => continue,
    };
    let used_mib: u64 = parts[2].parse().unwrap_or(0);
    out.push(GpuDevice {
      name,
      total_memory_bytes: total_mib.saturating_mul(1024 * 1024),
      used_memory_bytes: used_mib.saturating_mul(1024 * 1024),
    });
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parses_canonical_nvidia_smi_csv() {
    let stdout = "NVIDIA GeForce RTX 4090, 24564, 312\nNVIDIA GeForce RTX 4080, 16376, 0\n";
    let devices = parse(stdout);
    assert_eq!(devices.len(), 2);
    assert_eq!(devices[0].name, "NVIDIA GeForce RTX 4090");
    assert_eq!(devices[0].total_memory_bytes, 24564 * 1024 * 1024);
    assert_eq!(devices[0].used_memory_bytes, 312 * 1024 * 1024);
    assert_eq!(devices[1].total_memory_bytes, 16376 * 1024 * 1024);
  }

  #[test]
  fn empty_stdout_yields_no_devices() {
    assert!(parse("").is_empty());
    assert!(parse("\n   \n").is_empty());
  }

  #[test]
  fn malformed_rows_are_skipped() {
    let stdout = "bad row only\nNVIDIA RTX, 8192, 100\nnoise, also bad\n";
    let devices = parse(stdout);
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].name, "NVIDIA RTX");
  }
}
