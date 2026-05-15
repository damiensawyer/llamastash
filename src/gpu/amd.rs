//! AMD GPU probe via `rocm-smi --showmeminfo vram --json`.
//!
//! The JSON shape varies between ROCm releases; rather than pin to
//! one schema, we walk the response looking for any object with
//! `VRAM Total Memory (B)` and `VRAM Used Memory (B)` keys (or the
//! historical `vram total memory (B)` lower-case variant). That
//! survives ROCm 5/6 in our checks.

use std::process::Command;

use serde_json::Value;

use super::{GpuDevice, GpuInfo};

pub fn probe() -> Option<GpuInfo> {
  let output = Command::new("rocm-smi")
    .args(["--showmeminfo", "vram", "--json"])
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
  Some(GpuInfo::Amd { devices })
}

pub(crate) fn parse(stdout: &str) -> Vec<GpuDevice> {
  let v: Value = match serde_json::from_str(stdout) {
    Ok(v) => v,
    Err(_) => return Vec::new(),
  };
  let mut out = Vec::new();
  if let Some(obj) = v.as_object() {
    for (gpu_key, gpu_value) in obj {
      let Some(card) = gpu_value.as_object() else {
        continue;
      };
      let total = pick_u64(card, &["VRAM Total Memory (B)", "vram total memory (B)"]);
      let used = pick_u64(card, &["VRAM Used Memory (B)", "vram used memory (B)"]);
      if let Some(total_bytes) = total {
        out.push(GpuDevice {
          name: gpu_key.clone(),
          total_memory_bytes: total_bytes,
          used_memory_bytes: used.unwrap_or(0),
        });
      }
    }
  }
  out
}

fn pick_u64(card: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<u64> {
  for k in keys {
    if let Some(raw) = card.get(*k) {
      if let Some(n) = raw.as_u64() {
        return Some(n);
      }
      if let Some(s) = raw.as_str() {
        if let Ok(parsed) = s.parse::<u64>() {
          return Some(parsed);
        }
      }
    }
  }
  None
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parses_canonical_rocm_smi_output() {
    let stdout = r#"{
      "card0": {
        "VRAM Total Memory (B)": 17163091968,
        "VRAM Used Memory (B)": 256000000
      }
    }"#;
    let devices = parse(stdout);
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].name, "card0");
    assert_eq!(devices[0].total_memory_bytes, 17163091968);
    assert_eq!(devices[0].used_memory_bytes, 256000000);
  }

  #[test]
  fn falls_back_to_lowercase_key() {
    let stdout = r#"{
      "card0": { "vram total memory (B)": "1024", "vram used memory (B)": "512" }
    }"#;
    let devices = parse(stdout);
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].total_memory_bytes, 1024);
    assert_eq!(devices[0].used_memory_bytes, 512);
  }

  #[test]
  fn empty_or_invalid_json_yields_no_devices() {
    assert!(parse("").is_empty());
    assert!(parse("not json").is_empty());
    assert!(parse("{}").is_empty());
  }
}
