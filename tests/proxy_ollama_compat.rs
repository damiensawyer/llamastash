//! Integration tests for the proxy's Tier 1 Ollama-compat surface:
//! `/api/tags`, `/api/version`, `/api/ps`, `/api/show`.
//!
//! These exist so Ollama-shape discovery libraries (the `ollama-python`
//! default code path, IDE plugins probing `GET /api/tags`,
//! `OLLAMA_HOST` env-based detection) recognise llamastash as
//! Ollama-compatible and fall through to the OpenAI compat endpoints
//! for inference. The Tier 2 inference surface (`/api/chat`,
//! `/api/generate`, `/api/embed`) is deferred to a future plan — see
//! TODO §R2.
//!
//! Coverage shape mirrors `tests/proxy_models.rs`: hand-built
//! [`ProxyState`] with a seeded [`ModelCatalog`], proxy listener on an
//! ephemeral port, plain-TCP HTTP/1.1 client. No daemon bring-up
//! because the four endpoints are pure projections of catalog +
//! supervisor snapshots.

#![cfg(feature = "test-fixtures")]

use std::{
  net::{Ipv4Addr, SocketAddr, TcpListener as StdTcpListener},
  path::{Path, PathBuf},
  sync::Arc,
  time::Duration,
};

use llamastash::daemon::shutdown::ShutdownToken;
use llamastash::discovery::{DiscoveredModel, ModelCatalog, ModelSource};
use llamastash::gguf::metadata::{ModeHint, ModelMetadata, Quant};
use llamastash::ipc::methods::MethodContext;
use llamastash::proxy::server::{loopback_addr, new_status_cell, serve, ProxyStatus, StatusCell};
use llamastash::proxy::state::ProxyState;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::sleep;

// --- shared helpers ------------------------------------------------------
// Lifted from `proxy_models.rs` rather than shared via a `tests/common/`
// module to keep the integration tests self-contained — the per-file
// boilerplate is small enough that the duplication is cheaper than the
// indirection.

#[allow(dead_code)]
fn unique_temp_dir(label: &str) -> PathBuf {
  let suffix = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .expect("clock")
    .as_nanos();
  let dir = std::env::temp_dir().join(format!(
    "llamastash-proxyollama-{label}-{}-{suffix}",
    std::process::id()
  ));
  std::fs::create_dir_all(&dir).expect("temp dir creation");
  dir
}

#[allow(dead_code)]
fn pick_free_port() -> u16 {
  let l = StdTcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind ephemeral");
  l.local_addr().expect("local_addr").port()
}

async fn spawn_listener_with_state(
  state: Arc<ProxyState>,
) -> (SocketAddr, ShutdownToken, tokio::task::JoinHandle<()>) {
  let token = ShutdownToken::new();
  let status: StatusCell = new_status_cell();
  let bind_addr = loopback_addr(0);
  let token_for_task = token.clone();
  let status_for_task = Arc::clone(&status);
  let handle = tokio::spawn(async move {
    serve(state, bind_addr, token_for_task, status_for_task)
      .await
      .expect("proxy serve returns Ok");
  });
  let bound = wait_for_listening(&status, Duration::from_secs(2))
    .await
    .expect("listener reaches Listening");
  (bound, token, handle)
}

async fn shutdown_listener(shutdown: ShutdownToken, handle: tokio::task::JoinHandle<()>) {
  shutdown.trigger();
  tokio::time::timeout(Duration::from_secs(5), handle)
    .await
    .expect("proxy serve loop must exit after shutdown.trigger()")
    .expect("proxy serve task must not panic");
}

async fn wait_for_listening(status: &StatusCell, budget: Duration) -> Option<SocketAddr> {
  let deadline = std::time::Instant::now() + budget;
  while std::time::Instant::now() < deadline {
    if let ProxyStatus::Listening { addr } = status.read().unwrap().clone() {
      return Some(addr);
    }
    sleep(Duration::from_millis(10)).await;
  }
  None
}

async fn http_get(addr: SocketAddr, path: &str) -> (u16, Vec<u8>) {
  let mut sock = TcpStream::connect(addr).await.expect("connect");
  let req = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
  sock.write_all(req.as_bytes()).await.expect("write");
  let mut buf = Vec::new();
  sock.read_to_end(&mut buf).await.expect("read");
  parse_response(&buf)
}

async fn http_post(addr: SocketAddr, path: &str, body: &str) -> (u16, Vec<u8>) {
  let mut sock = TcpStream::connect(addr).await.expect("connect");
  let req = format!(
    "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Length: {len}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
    len = body.len()
  );
  sock.write_all(req.as_bytes()).await.expect("write");
  let mut buf = Vec::new();
  sock.read_to_end(&mut buf).await.expect("read");
  parse_response(&buf)
}

fn parse_response(buf: &[u8]) -> (u16, Vec<u8>) {
  let needle = b"\r\n\r\n";
  let split = buf
    .windows(needle.len())
    .position(|w| w == needle)
    .expect("CRLFCRLF terminator");
  let head = std::str::from_utf8(&buf[..split]).expect("utf8 headers");
  let status: u16 = head
    .lines()
    .next()
    .expect("status line")
    .split_whitespace()
    .nth(1)
    .expect("status code")
    .parse()
    .expect("parse status");
  let body = buf[split + needle.len()..].to_vec();
  (status, body)
}

fn fake_metadata(arch: &str, mode: ModeHint) -> ModelMetadata {
  ModelMetadata {
    arch: Some(arch.to_string()),
    total_parameters: Some(7_000_000_000),
    parameter_label: Some("7B".to_string()),
    quant: Quant::Q4_K,
    native_ctx: Some(8192),
    chat_template: Some("{{ messages }}".to_string()),
    tokenizer_kind: Some("llama".to_string()),
    reasoning_hint: false,
    mode_hint: mode,
    weights_bytes: Some(4_200_000_000),
  }
}

fn make_model(
  path: &str,
  display_label: Option<&str>,
  arch: &str,
  mode: ModeHint,
) -> DiscoveredModel {
  let p = PathBuf::from(path);
  let parent = p.parent().unwrap_or(Path::new("/")).to_path_buf();
  DiscoveredModel {
    path: p,
    parent,
    source: ModelSource::UserPath,
    metadata: Some(fake_metadata(arch, mode)),
    parse_error: None,
    split_siblings: Vec::new(),
    display_label: display_label.map(str::to_string),
  }
}

fn make_parse_error_model(path: &str) -> DiscoveredModel {
  let p = PathBuf::from(path);
  let parent = p.parent().unwrap_or(Path::new("/")).to_path_buf();
  DiscoveredModel {
    path: p,
    parent,
    source: ModelSource::UserPath,
    metadata: None,
    parse_error: Some("synthetic parse failure".to_string()),
    split_siblings: Vec::new(),
    display_label: None,
  }
}

async fn proxy_state_with_models(models: Vec<DiscoveredModel>) -> Arc<ProxyState> {
  let catalog = ModelCatalog::new();
  for m in models {
    catalog.upsert(m).await;
  }
  let ctx = MethodContext::with_catalog(ShutdownToken::new(), catalog);
  ProxyState::from_context(&ctx)
}

// --- /api/version --------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_version_returns_cargo_pkg_version() {
  let state = proxy_state_with_models(Vec::new()).await;
  let (addr, shutdown, handle) = spawn_listener_with_state(state).await;

  let (status, body) = http_get(addr, "/api/version").await;
  assert_eq!(status, 200);
  let v: Value = serde_json::from_slice(&body).expect("json body");
  let version = v
    .get("version")
    .and_then(Value::as_str)
    .expect("version field");
  // Cargo.toml sets the crate version; this asserts the field is
  // present and non-empty without pinning a specific value (which
  // would churn on every release).
  assert!(
    !version.is_empty(),
    "version field must be non-empty: {version}"
  );
  // Exactly one field on the wire — discovery clients pin against
  // this shape.
  let obj = v.as_object().expect("object body");
  assert_eq!(obj.len(), 1, "wire shape: {v}");

  shutdown_listener(shutdown, handle).await;
}

// --- /api/tags -----------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_tags_returns_alphabetical_models_with_documented_fields() {
  let models = vec![
    make_model("/m/qwen3.gguf", Some("qwen3"), "qwen3", ModeHint::Chat),
    make_model("/m/llama.gguf", None, "llama", ModeHint::Chat),
    make_model("/m/gemma.gguf", Some("gemma:2b"), "gemma", ModeHint::Chat),
  ];
  let state = proxy_state_with_models(models).await;
  let (addr, shutdown, handle) = spawn_listener_with_state(state).await;

  let (status, body) = http_get(addr, "/api/tags").await;
  assert_eq!(status, 200);
  let v: Value = serde_json::from_slice(&body).expect("json body");
  let arr = v["models"].as_array().expect("models array");
  assert_eq!(arr.len(), 3);
  let names: Vec<&str> = arr.iter().map(|r| r["name"].as_str().unwrap()).collect();
  assert_eq!(
    names,
    ["gemma:2b", "llama", "qwen3"],
    "models sorted alphabetically"
  );
  // Per-row shape check: required Ollama fields present.
  let first = &arr[0];
  assert!(first["name"].is_string());
  assert!(first["model"].is_string());
  assert_eq!(
    first["name"], first["model"],
    "name and model agree on local-only rows"
  );
  assert!(first["modified_at"].is_string());
  assert!(first["size"].is_u64());
  let digest = first["digest"].as_str().expect("digest field");
  assert!(
    digest.starts_with("blake3:"),
    "digest uses blake3 prefix: {digest}"
  );
  let details = &first["details"];
  assert_eq!(details["format"], "gguf");
  assert_eq!(details["family"], "gemma");
  assert!(details["families"].is_array());
  assert_eq!(details["quantization_level"], "Q4_K");
  assert_eq!(details["parameter_size"], "7B");

  shutdown_listener(shutdown, handle).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_tags_empty_catalog_returns_empty_models_list() {
  let state = proxy_state_with_models(Vec::new()).await;
  let (addr, shutdown, handle) = spawn_listener_with_state(state).await;

  let (status, body) = http_get(addr, "/api/tags").await;
  assert_eq!(status, 200);
  let v: Value = serde_json::from_slice(&body).expect("json body");
  let arr = v["models"].as_array().expect("models array");
  assert_eq!(arr.len(), 0, "empty catalog → empty list, not 404");

  shutdown_listener(shutdown, handle).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_tags_tolerates_parse_error_rows() {
  // A discovery row with `metadata: None` (header parse failed) must
  // still surface in the tags list — Ollama clients that pin against
  // /api/tags shouldn't see disappearing models on transient header
  // problems. The row carries empty `family` / `parameter_size` and
  // an `Unknown` quantization.
  let models = vec![
    make_model("/m/good.gguf", None, "llama", ModeHint::Chat),
    make_parse_error_model("/m/broken.gguf"),
  ];
  let state = proxy_state_with_models(models).await;
  let (addr, shutdown, handle) = spawn_listener_with_state(state).await;

  let (status, body) = http_get(addr, "/api/tags").await;
  assert_eq!(status, 200);
  let v: Value = serde_json::from_slice(&body).expect("json body");
  let arr = v["models"].as_array().expect("models array");
  assert_eq!(arr.len(), 2);
  let broken = arr
    .iter()
    .find(|r| r["name"] == "broken")
    .expect("broken row present");
  assert_eq!(broken["details"]["family"], "");
  assert_eq!(broken["details"]["quantization_level"], "Unknown");
  assert_eq!(broken["size"], 0);

  shutdown_listener(shutdown, handle).await;
}

// --- /api/ps -------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_ps_with_no_running_supervisors_returns_empty_list() {
  // Catalog has models but no supervisor is Ready → /api/ps is empty.
  // Mirrors Ollama's behaviour: /api/tags lists files on disk,
  // /api/ps lists loaded models.
  let models = vec![make_model("/m/dormant.gguf", None, "llama", ModeHint::Chat)];
  let state = proxy_state_with_models(models).await;
  let (addr, shutdown, handle) = spawn_listener_with_state(state).await;

  let (status, body) = http_get(addr, "/api/ps").await;
  assert_eq!(status, 200);
  let v: Value = serde_json::from_slice(&body).expect("json body");
  let arr = v["models"].as_array().expect("models array");
  assert_eq!(arr.len(), 0);

  shutdown_listener(shutdown, handle).await;
}

// --- /api/show -----------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_show_returns_metadata_for_known_model() {
  let models = vec![make_model(
    "/m/qwen-coder.gguf",
    Some("qwen-coder:7b"),
    "qwen3",
    ModeHint::Chat,
  )];
  let state = proxy_state_with_models(models).await;
  let (addr, shutdown, handle) = spawn_listener_with_state(state).await;

  let (status, body) = http_post(addr, "/api/show", r#"{"model":"qwen-coder:7b"}"#).await;
  assert_eq!(status, 200);
  let v: Value = serde_json::from_slice(&body).expect("json body");
  // Documented top-level slots present.
  assert!(v["modelfile"].is_string(), "modelfile field present");
  assert!(v["parameters"].is_string(), "parameters field present");
  assert_eq!(v["template"], "{{ messages }}");
  assert_eq!(v["details"]["family"], "qwen3");
  assert_eq!(v["details"]["quantization_level"], "Q4_K");
  // model_info carries the typical Ollama-shape keys.
  let info = v["model_info"].as_object().expect("model_info object");
  assert_eq!(
    info.get("general.architecture"),
    Some(&Value::from("qwen3"))
  );
  assert_eq!(
    info.get("general.parameter_count"),
    Some(&Value::from(7_000_000_000_u64))
  );
  assert_eq!(
    info.get("general.parameter_label"),
    Some(&Value::from("7B"))
  );
  assert_eq!(info.get("general.context_length"), Some(&Value::from(8192)));
  // capabilities reflects Chat mode_hint.
  let caps = v["capabilities"].as_array().expect("capabilities array");
  let caps_str: Vec<&str> = caps.iter().filter_map(Value::as_str).collect();
  assert_eq!(caps_str, vec!["completion"]);

  shutdown_listener(shutdown, handle).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_show_accepts_legacy_name_field() {
  // Older Ollama clients send `{"name": "model-name"}`; the proxy
  // must accept either.
  let models = vec![make_model("/m/legacy.gguf", None, "llama", ModeHint::Chat)];
  let state = proxy_state_with_models(models).await;
  let (addr, shutdown, handle) = spawn_listener_with_state(state).await;

  let (status, body) = http_post(addr, "/api/show", r#"{"name":"legacy"}"#).await;
  assert_eq!(status, 200);
  let v: Value = serde_json::from_slice(&body).expect("json body");
  assert_eq!(v["details"]["family"], "llama");

  shutdown_listener(shutdown, handle).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_show_missing_model_returns_404_model_not_found() {
  let state = proxy_state_with_models(Vec::new()).await;
  let (addr, shutdown, handle) = spawn_listener_with_state(state).await;

  let (status, body) = http_post(addr, "/api/show", r#"{"model":"nonexistent"}"#).await;
  assert_eq!(status, 404);
  let v: Value = serde_json::from_slice(&body).expect("json body");
  assert_eq!(v["error"]["type"], "model_not_found");

  shutdown_listener(shutdown, handle).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_show_without_model_field_returns_400_model_required() {
  let state = proxy_state_with_models(Vec::new()).await;
  let (addr, shutdown, handle) = spawn_listener_with_state(state).await;

  let (status, body) = http_post(addr, "/api/show", r#"{}"#).await;
  assert_eq!(status, 400);
  let v: Value = serde_json::from_slice(&body).expect("json body");
  assert_eq!(v["error"]["type"], "invalid_request");
  assert_eq!(v["error"]["code"], "model_required");

  shutdown_listener(shutdown, handle).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_show_ambiguous_reference_returns_400_with_matches() {
  // Two rows both contain "qwen" — substring lookup is ambiguous,
  // mirroring the OpenAI compat surface's ambiguous_model branch.
  let models = vec![
    make_model(
      "/m/qwen3-7b.gguf",
      Some("qwen3:7b"),
      "qwen3",
      ModeHint::Chat,
    ),
    make_model(
      "/m/qwen3-13b.gguf",
      Some("qwen3:13b"),
      "qwen3",
      ModeHint::Chat,
    ),
  ];
  let state = proxy_state_with_models(models).await;
  let (addr, shutdown, handle) = spawn_listener_with_state(state).await;

  let (status, body) = http_post(addr, "/api/show", r#"{"model":"qwen3"}"#).await;
  assert_eq!(status, 400);
  let v: Value = serde_json::from_slice(&body).expect("json body");
  assert_eq!(v["error"]["type"], "ambiguous_model");
  let matches = v["error"]["matches"].as_array().expect("matches array");
  assert_eq!(matches.len(), 2);

  shutdown_listener(shutdown, handle).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_show_capabilities_reflect_mode_hint() {
  let models = vec![
    make_model(
      "/m/embed.gguf",
      Some("embed-mini"),
      "bert",
      ModeHint::Embedding,
    ),
    make_model(
      "/m/rerank.gguf",
      Some("rerank-bge"),
      "bert",
      ModeHint::Rerank,
    ),
  ];
  let state = proxy_state_with_models(models).await;
  let (addr, shutdown, handle) = spawn_listener_with_state(state).await;

  let (status, body) = http_post(addr, "/api/show", r#"{"model":"embed-mini"}"#).await;
  assert_eq!(status, 200);
  let v: Value = serde_json::from_slice(&body).expect("json body");
  let caps: Vec<&str> = v["capabilities"]
    .as_array()
    .expect("array")
    .iter()
    .filter_map(Value::as_str)
    .collect();
  assert_eq!(caps, vec!["embedding"], "embedding mode → embedding cap");

  let (status, body) = http_post(addr, "/api/show", r#"{"model":"rerank-bge"}"#).await;
  assert_eq!(status, 200);
  let v: Value = serde_json::from_slice(&body).expect("json body");
  let caps: Vec<&str> = v["capabilities"]
    .as_array()
    .expect("array")
    .iter()
    .filter_map(Value::as_str)
    .collect();
  assert_eq!(caps, vec!["rerank"], "rerank mode → rerank cap");

  shutdown_listener(shutdown, handle).await;
}
