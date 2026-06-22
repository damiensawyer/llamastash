//! Post-launch actuals: what `--fit` actually chose, read once from the
//! child's `/props` after it reaches Ready.
//!
//! Since placement is delegated to `--fit`, llamastash does not know the
//! resolved context window (or layer split) until the child is up. We
//! fetch it once on the Loading→Ready transition and surface it on
//! `status` (and thus the TUI Running view + `show`). Best-effort: a
//! build whose `/props` omits the field, or any transport error, yields
//! `None` and the surfaces render "unavailable" rather than a wrong
//! number.
//!
//! Transport is a hand-rolled `GET /props` over raw TCP with
//! `Connection: close` (so we can read to EOF without a keep-alive
//! dance) — the same no-dep stance as [`crate::daemon::probe`]. We never
//! pull in `reqwest`/`hyper` just for this.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// What the child reports it actually loaded with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Actuals {
  /// Resolved **per-request** context window the child loaded with —
  /// the window a single sequence/conversation can actually use, what
  /// `--fit` (or a pin) settled on. `None` when `/props` didn't expose
  /// it. This is the one placement value llama-server's HTTP API
  /// reports; the rest (layers, threads, batch) live only in load-time
  /// logs, so the TUI shows those as `auto`.
  ///
  /// Read straight from `/props` `default_generation_settings.n_ctx` —
  /// **not** multiplied by `total_slots`. That field already is the
  /// per-request window in both slot modes:
  /// - **non-unified** (explicit `--parallel N`): `n_ctx` is per-slot
  ///   (`total / N`), which is exactly what one request gets.
  /// - **unified** (`--parallel` auto → `kv_unified = true`, the
  ///   default): `n_ctx` is the full shared window, and a request can
  ///   use all of it.
  ///
  /// An earlier version multiplied by `total_slots` to recover the `-c`
  /// aggregate, but that double-counted under the default kv-unified
  /// mode (e.g. `-c 8192` auto → `/props n_ctx=8192, total_slots=4`,
  /// which is `8192`, not `32768`) and could report a window larger
  /// than the model's trained context. The per-request value is both
  /// correct and what users mean by "context window".
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub resolved_ctx: Option<u32>,

  /// True when `--fit` had to clamp the context window down to the
  /// `--fit-ctx` floor even though the model's trained window is larger
  /// — i.e. memory pressure, not the model's own limit. Computed by the
  /// supervisor's readiness gate (it needs the floor + the trained
  /// window, neither of which `/props` reports), not parsed from
  /// `/props`. Drives the strict-fit refusal and the soft "ctx clamped"
  /// notice on the running surfaces.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub ctx_clamped: bool,
}

impl Actuals {
  /// True when nothing was captured — surfaces render "unavailable".
  /// Keyed on `resolved_ctx` alone: `ctx_clamped` is a derived flag that
  /// is only meaningful once a context window was resolved.
  pub fn is_empty(&self) -> bool {
    self.resolved_ctx.is_none()
  }
}

/// Fetch `/props` from the child on `127.0.0.1:<port>` and extract the
/// values `--fit` resolved. Best-effort: any error → empty `Actuals`.
pub async fn fetch(port: u16, timeout: Duration) -> Actuals {
  match fetch_props_body(port, timeout).await {
    Ok(body) => parse_actuals(&body),
    Err(_) => Actuals::default(),
  }
}

async fn fetch_props_body(port: u16, timeout: Duration) -> std::io::Result<Vec<u8>> {
  let request = format!(
    "GET /props HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nAccept: application/json\r\n\r\n"
  );
  let fut = async {
    let mut sock = TcpStream::connect(("127.0.0.1", port)).await?;
    sock.write_all(request.as_bytes()).await?;
    let mut buf = Vec::with_capacity(4096);
    // `Connection: close` makes the server hang up after the body, so a
    // read-to-EOF terminates. Cap the buffer so a misbehaving peer can't
    // stream unbounded data into the daemon.
    let mut chunk = [0u8; 4096];
    loop {
      let n = sock.read(&mut chunk).await?;
      if n == 0 {
        break;
      }
      buf.extend_from_slice(&chunk[..n]);
      if buf.len() > 256 * 1024 {
        break;
      }
    }
    Ok::<_, std::io::Error>(buf)
  };
  tokio::time::timeout(timeout, fut)
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "props fetch timeout"))?
}

/// Split the HTTP response at the header/body boundary and parse the
/// JSON body for the resolved context window. llama-server carries the
/// per-request `n_ctx` under `default_generation_settings` (and at the
/// top level in some builds); we read it verbatim — see
/// [`extract_n_ctx`] and the `Actuals::resolved_ctx` doc for why
/// `total_slots` is deliberately *not* a factor.
fn parse_actuals(response: &[u8]) -> Actuals {
  let Some(split) = response.windows(4).position(|w| w == b"\r\n\r\n") else {
    return Actuals::default();
  };
  let body = &response[split + 4..];
  let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
    return Actuals::default();
  };
  Actuals {
    resolved_ctx: extract_n_ctx(&v),
    // `/props` can't tell us this; the readiness gate computes it from
    // the floor + the trained window. Always false out of the parser.
    ctx_clamped: false,
  }
}

/// Per-request context window from `/props` — the window one sequence
/// can use. This is `default_generation_settings.n_ctx` read verbatim:
/// per-slot in non-unified mode and the full shared window under the
/// default kv-unified mode, which is the per-request size either way.
/// Multiplying by `total_slots` would double-count under kv-unified and
/// can exceed the model's trained window, so we never do.
fn extract_n_ctx(v: &serde_json::Value) -> Option<u32> {
  v.get("default_generation_settings")
    .and_then(|g| g.get("n_ctx"))
    .and_then(serde_json::Value::as_u64)
    .or_else(|| v.get("n_ctx").and_then(serde_json::Value::as_u64))
    .map(|n| n as u32)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parses_n_ctx_from_default_generation_settings() {
    let resp = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
      {\"default_generation_settings\":{\"n_ctx\":16384},\"total_slots\":1}";
    assert_eq!(parse_actuals(resp).resolved_ctx, Some(16384));
  }

  #[test]
  fn falls_back_to_top_level_n_ctx() {
    let resp = b"HTTP/1.1 200 OK\r\n\r\n{\"n_ctx\":8192}";
    assert_eq!(parse_actuals(resp).resolved_ctx, Some(8192));
  }

  #[test]
  fn resolved_ctx_reads_n_ctx_verbatim_ignoring_slots() {
    // `default_generation_settings.n_ctx` is the per-request window; we
    // report it as-is and never multiply by `total_slots`.
    //
    // Non-unified (explicit `--parallel 4`, `-c 8192`): n_ctx is the
    // per-slot 2048 — what one request gets — so we report 2048, not
    // the 8192 aggregate.
    let non_unified = b"HTTP/1.1 200 OK\r\n\r\n\
      {\"default_generation_settings\":{\"n_ctx\":2048},\"total_slots\":4}";
    assert_eq!(parse_actuals(non_unified).resolved_ctx, Some(2048));
    // Unified (auto `--parallel` → kv_unified, the default): `/props`
    // reports the full shared window. Verified live against `-c 8192`
    // auto: n_ctx=8192, total_slots=4. The old `x slots` logic wrongly
    // produced 32768 (and 524288 for the 80B); we now report 8192.
    let unified = b"HTTP/1.1 200 OK\r\n\r\n\
      {\"default_generation_settings\":{\"n_ctx\":8192},\"total_slots\":4}";
    assert_eq!(parse_actuals(unified).resolved_ctx, Some(8192));
  }

  #[test]
  fn missing_field_yields_none_not_a_crash() {
    let resp = b"HTTP/1.1 200 OK\r\n\r\n{\"model_path\":\"x\"}";
    assert_eq!(parse_actuals(resp).resolved_ctx, None);
  }

  #[test]
  fn malformed_body_yields_none() {
    assert!(parse_actuals(b"HTTP/1.1 200 OK\r\n\r\nnot json").is_empty());
    assert!(parse_actuals(b"no headers no body").is_empty());
  }

  #[test]
  fn actuals_is_empty_when_unset() {
    assert!(Actuals::default().is_empty());
    assert!(!Actuals {
      resolved_ctx: Some(4096),
      ctx_clamped: false,
    }
    .is_empty());
  }
}
