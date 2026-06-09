//! Lemonade discovery source (R11) — **list-only**.
//!
//! Reads the model list from a running `lemond` umbrella's `/api/v1/models`
//! and projects each entry into a Lemonade-tagged [`DiscoveredModel`].
//!
//! **Acquisition is Lemonade's job** (see the plan's Scope Boundaries): this
//! source only *lists* what `lemond` already knows — it never downloads or
//! pulls. Users acquire models via `lemonade pull <model>` or the Lemonade
//! web UI. Best-effort: a transport error (umbrella not up) yields no rows
//! and never aborts the surrounding scan, so a disabled/absent Lemonade
//! backend degrades cleanly to "no Lemonade rows".

use std::path::PathBuf;

use crate::backend::lemonade::LemonadeClient;
use crate::discovery::{DiscoveredModel, ModelSource};

/// Synthetic path for a Lemonade-registry model (no local file). Keeps the
/// catalog's path-keyed map unique per model; the user-facing name lives in
/// `display_label` and is what resolution / routing key on.
fn synthetic_path(name: &str) -> PathBuf {
  PathBuf::from(format!("lemonade://{name}"))
}

/// Project one Lemonade registry name into a catalog row.
fn row_for(name: &str) -> DiscoveredModel {
  DiscoveredModel {
    path: synthetic_path(name),
    parent: PathBuf::from("lemonade://"),
    source: ModelSource::Lemonade,
    metadata: None,
    parse_error: None,
    split_siblings: Vec::new(),
    display_label: Some(name.to_string()),
    // Lemonade serves registry models by name, not local GGUFs — there's no
    // companion projector to detect, so no multimodal signal.
    multimodal: None,
  }
}

/// Enumerate the models a `lemond` umbrella on `port` reports. Best-effort:
/// returns an empty vec (never errors) when the umbrella is unreachable.
pub async fn enumerate(port: u16) -> Vec<DiscoveredModel> {
  let client = match LemonadeClient::new(port) {
    Ok(c) => c,
    Err(e) => {
      log::debug!("lemonade discovery: client build failed: {e}");
      return Vec::new();
    }
  };
  match client.list_models().await {
    Ok(names) => names.iter().map(|n| row_for(n)).collect(),
    Err(e) => {
      log::debug!("lemonade discovery: list_models failed (umbrella down?): {e}");
      Vec::new()
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::TcpListener;

  /// Spawn a loopback fake serving `GET /api/v1/models` with the given
  /// OpenAI-list body; one connection per accept until dropped.
  async fn spawn_fake_models(body: &'static str) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
      loop {
        let Ok((mut sock, _)) = listener.accept().await else {
          break;
        };
        let mut buf = vec![0u8; 2048];
        let _ = sock.read(&mut buf).await;
        let resp = format!(
          "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
          body.len()
        );
        let _ = sock.write_all(resp.as_bytes()).await;
      }
    });
    port
  }

  #[tokio::test]
  async fn enumerate_projects_models_into_lemonade_rows() {
    let port = spawn_fake_models(
      r#"{"object":"list","data":[{"id":"Qwen2.5-0.5B-Instruct"},{"id":"Llama-3.1-8B"}]}"#,
    )
    .await;
    let rows = enumerate(port).await;
    let names: Vec<String> = rows
      .iter()
      .map(|r| r.display_label.clone().unwrap())
      .collect();
    assert_eq!(names, vec!["Qwen2.5-0.5B-Instruct", "Llama-3.1-8B"]);
    // Every row is tagged Lemonade with a synthetic (file-less) path.
    assert!(rows.iter().all(|r| r.source == ModelSource::Lemonade));
    assert!(rows.iter().all(|r| r.metadata.is_none()));
    assert_eq!(
      rows[0].path,
      PathBuf::from("lemonade://Qwen2.5-0.5B-Instruct")
    );
    // The backend tag derives from the source (R13/R14).
    assert_eq!(rows[0].source.backend_id(), "lemonade");
  }

  #[tokio::test]
  async fn enumerate_returns_empty_when_umbrella_unreachable() {
    // Port 1 has nothing listening → transport error → no rows, no panic.
    let rows = enumerate(1).await;
    assert!(rows.is_empty());
  }
}
