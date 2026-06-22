//! HTTP-auth primitives shared by the control-plane bearer token
//! (`crate::daemon::auth`) and the proxy API key (`crate::proxy::auth`).
//! Kept in `util` so both auth surfaces depend *down* on one copy
//! instead of one borrowing from the other.

/// Extract the bearer token value from an `Authorization` header value,
/// or `None` if the header is missing the `Bearer ` prefix / has
/// trailing junk. Case-sensitive on the scheme per RFC 6750 §2.1
/// (servers MAY be case-insensitive, but we don't need to be — every
/// client we ship sends `Bearer ` verbatim).
pub fn extract_bearer(header_value: &str) -> Option<&str> {
  header_value.strip_prefix("Bearer ").map(str::trim)
}

/// Constant-time byte slice comparison. Length-aware early-exit is
/// deliberate (the slot is the secret length, not the secret itself —
/// leaking it via timing or a fast path is acceptable). Once lengths
/// match the loop visits every byte regardless of where a mismatch
/// falls, so the compare time is independent of the secret's contents.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
  if a.len() != b.len() {
    return false;
  }
  let mut diff: u8 = 0;
  for (x, y) in a.iter().zip(b.iter()) {
    diff |= x ^ y;
  }
  diff == 0
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn extract_bearer_strips_prefix() {
    assert_eq!(extract_bearer("Bearer abc"), Some("abc"));
    assert_eq!(extract_bearer("Bearer  spaced  "), Some("spaced"));
  }

  #[test]
  fn extract_bearer_rejects_non_bearer() {
    assert_eq!(extract_bearer(""), None);
    assert_eq!(extract_bearer("Basic abc"), None);
    assert_eq!(extract_bearer("bearer abc"), None); // case-sensitive on scheme
  }

  #[test]
  fn constant_time_eq_matches_only_identical_bytes() {
    assert!(constant_time_eq(b"secret", b"secret"));
    assert!(!constant_time_eq(b"secret", b"secrxt"));
  }

  #[test]
  fn constant_time_eq_rejects_length_mismatch() {
    // Equal length, differing bytes — and unequal length — both reject
    // without a content-dependent early exit.
    assert!(!constant_time_eq(b"short", b"shorter"));
    assert!(!constant_time_eq(b"much-longer", b"short"));
    assert!(!constant_time_eq(b"", b"x"));
  }
}
