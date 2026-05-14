//! Stable model identity = (canonical path, BLAKE3 of header bytes).
//!
//! Why a header hash instead of a whole-file hash:
//! - Whole-file hashing of a 7B GGUF is ~5 GB of disk I/O. The launcher
//!   touches identity on every scan, which would brick discovery.
//! - The header is small (<1 MiB typical) and is the part of the file that
//!   uniquely identifies the model (arch, tensors layout, quant tags).
//! - Identity must survive a `mv` of the file. Path-only identity does not;
//!   header-hash + canonical-path lets us detect a renamed file and fold
//!   its last-params (Unit 5) onto the new path.

use std::path::{Path, PathBuf};

/// Stable identifier for a single GGUF on disk.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelId {
  /// Canonical absolute path (`std::fs::canonicalize`).
  pub path: PathBuf,
  /// BLAKE3 hash of the structural header bytes (the `raw` field returned
  /// by [`crate::gguf::header::read_path`]).
  pub header_blake3: [u8; 32],
}

impl ModelId {
  /// Lower-case hex view of the BLAKE3 digest, suitable for filenames and
  /// log lines.
  pub fn header_hex(&self) -> String {
    let mut out = String::with_capacity(64);
    for byte in &self.header_blake3 {
      out.push_str(&format!("{byte:02x}"));
    }
    out
  }

  /// Short fingerprint (first 8 hex chars). Used in CLI output and TUI
  /// status rows where the full 64-char digest is too noisy.
  pub fn short_fingerprint(&self) -> String {
    let mut out = String::with_capacity(8);
    for byte in self.header_blake3.iter().take(4) {
      out.push_str(&format!("{byte:02x}"));
    }
    out
  }
}

/// Compute a [`ModelId`] from the supplied path and the raw header bytes
/// returned by [`crate::gguf::header::read_path`].
///
/// `path` is canonicalised via [`std::fs::canonicalize`] when possible;
/// when the file does not exist (in tests that build only an in-memory
/// header), we fall back to the path as supplied.
pub fn compute<P: AsRef<Path>>(path: P, header_bytes: &[u8]) -> ModelId {
  let canonical =
    std::fs::canonicalize(path.as_ref()).unwrap_or_else(|_| path.as_ref().to_path_buf());
  let digest = blake3::hash(header_bytes);
  ModelId {
    path: canonical,
    header_blake3: *digest.as_bytes(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::fs;

  #[test]
  fn same_bytes_yield_same_hash() {
    let bytes = b"GGUF\x03\x00\x00\x00";
    let a = compute("/tmp/llamatui-fake-a.gguf", bytes);
    let b = compute("/tmp/llamatui-fake-b.gguf", bytes);
    assert_eq!(a.header_blake3, b.header_blake3);
    assert_ne!(a.path, b.path);
  }

  #[test]
  fn hash_is_stable_across_rename() {
    let dir = tempdir_for_test();
    let a = dir.join("alpha.gguf");
    let b = dir.join("beta.gguf");
    let bytes = b"GGUF\x03\x00\x00\x00 some header payload".to_vec();
    fs::write(&a, &bytes).unwrap();
    let id_a = compute(&a, &bytes);
    fs::rename(&a, &b).unwrap();
    let id_b = compute(&b, &bytes);
    assert_eq!(id_a.header_blake3, id_b.header_blake3);
    assert_ne!(id_a.path, id_b.path);
  }

  #[test]
  fn short_fingerprint_is_eight_hex_chars() {
    let id = compute("/tmp/x.gguf", b"abc");
    assert_eq!(id.short_fingerprint().len(), 8);
    assert!(id
      .short_fingerprint()
      .chars()
      .all(|c| c.is_ascii_hexdigit()));
  }

  fn tempdir_for_test() -> std::path::PathBuf {
    let base = std::env::temp_dir().join(format!(
      "llamatui-id-{}-{}",
      std::process::id(),
      std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
    ));
    fs::create_dir_all(&base).unwrap();
    base
  }
}
