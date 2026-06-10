//! Managed-multiplexer integration: the generic supervisor spawns the
//! `lemond` umbrella from a `LemonadeBackend`-produced spec, reaches `/live`
//! readiness, and the typed client talks to the running umbrella — the
//! headline Phase 2 capability (Lemonade reachable through llamastash)
//! proven end-to-end against the `fake_lemond` fixture (no real `lemond`
//! or NPU needed).
//!
//! Per-model routing (a Lemonade model in the catalog + proxy forwarding to
//! the umbrella) is exercised in `lemonade_route_test.rs`.
#![cfg(feature = "test-fixtures")]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use llamastash::backend::identity::{BackendModelId, ModelIdentity};
use llamastash::backend::lemonade::{
  ensure_umbrella, umbrella_launch_id, LemonadeBackend, LemonadeClient,
};
use llamastash::backend::{Backend, LaunchPlan};
use llamastash::daemon::probe::ProbeOptions;
use llamastash::daemon::registry::SupervisorRegistry;
use llamastash::daemon::shutdown::ShutdownToken;
use llamastash::daemon::state_store::RunningSnapshot;
use llamastash::daemon::supervisor::{ManagedModel, ManagedState};
use llamastash::ipc::methods::{dispatch_request, MethodContext};
use llamastash::ipc::protocol::Request;
use llamastash::launch::mode::LaunchMode;
use llamastash::launch::params::LaunchParams;
use serde_json::json;

fn fake_lemond_binary() -> PathBuf {
  PathBuf::from(env!("CARGO_BIN_EXE_fake_lemond"))
}

fn unique_temp(label: &str) -> PathBuf {
  llamastash::test_support::unique_temp_dir("ls-lemon", label)
}

fn allocate_port() -> u16 {
  let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
  let port = listener.local_addr().unwrap().port();
  drop(listener);
  port
}

fn fast_probe() -> ProbeOptions {
  ProbeOptions {
    interval: Duration::from_millis(40),
    timeout: Duration::from_secs(5),
  }
}

/// `supervisor::spawn` returns at `Loading` and flips to `Ready` from its
/// background probe task; poll until the umbrella's `/live` probe succeeds.
async fn wait_ready(model: &ManagedModel) {
  let deadline = Instant::now() + Duration::from_secs(5);
  loop {
    match model.state().await {
      ManagedState::Ready => return,
      ManagedState::Error { cause } => panic!("umbrella errored: {cause}"),
      other => {
        assert!(
          Instant::now() < deadline,
          "umbrella not ready in time: {other:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
      }
    }
  }
}

/// Build the umbrella spec a `LemonadeBackend` would produce, pointed at the
/// `fake_lemond` binary on `port`.
fn umbrella_spec(port: u16) -> llamastash::backend::ProcessLaunchSpec {
  let params = LaunchParams::new(PathBuf::from("Qwen2.5-0.5B-Instruct"), LaunchMode::Chat);
  let plan =
    LemonadeBackend::new().prepare_launch(&params, port, fake_lemond_binary(), fast_probe());
  match plan {
    LaunchPlan::DelegateToManager(spec) => spec.umbrella,
    LaunchPlan::SpawnProcess(_) => panic!("lemonade must produce a DelegateToManager plan"),
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervisor_spawns_lemond_umbrella_and_client_talks_to_it() {
  let logs = unique_temp("logs");
  std::fs::create_dir_all(&logs).unwrap();
  let registry = SupervisorRegistry::new();
  let port = allocate_port();

  // Ensure the umbrella: the generic supervisor spawns fake_lemond and
  // blocks until /live returns 200 (readiness from the LemonadeBackend spec).
  let model = ensure_umbrella(
    &registry,
    port,
    umbrella_spec(port),
    logs.join("lemond.log"),
  )
  .await
  .expect("umbrella should spawn");
  wait_ready(&model).await;
  assert_eq!(model.port(), port);

  // The typed client can now talk to the running umbrella.
  let client = LemonadeClient::new(port).expect("client");
  client.live().await.expect("/live reachable");
  let models = client.list_models().await.expect("models list");
  assert!(
    models.iter().any(|m| m == "Qwen2.5-0.5B-Instruct"),
    "fake lemond should list its models, got {models:?}"
  );
  client.load("Qwen2.5-0.5B-Instruct").await.expect("load ok");

  // Stop the umbrella so the spawned `fake_lemond` child doesn't outlive the
  // test. On Windows a leaked child holding inherited stdio handles makes
  // `cargo test` hang at exit; on Unix it's harmless but we clean up anyway.
  model.stop(Duration::from_secs(3)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ensure_umbrella_is_idempotent() {
  let logs = unique_temp("logs2");
  std::fs::create_dir_all(&logs).unwrap();
  let registry = SupervisorRegistry::new();
  let port = allocate_port();

  let first = ensure_umbrella(&registry, port, umbrella_spec(port), logs.join("a.log"))
    .await
    .expect("first ensure");
  // A second ensure must reuse the registered umbrella, not spawn another.
  // Pass a different port to prove it is ignored when one already exists.
  let second = ensure_umbrella(
    &registry,
    allocate_port(),
    umbrella_spec(port),
    logs.join("b.log"),
  )
  .await
  .expect("second ensure");

  assert_eq!(
    first.port(),
    second.port(),
    "reused umbrella keeps its port"
  );
  let snapshot = registry.snapshot().await;
  let umbrellas = snapshot
    .iter()
    .filter(|(id, _)| *id == umbrella_launch_id())
    .count();
  assert_eq!(umbrellas, 1, "exactly one umbrella should be registered");

  // `first` and `second` are the same reused umbrella — stop it once so the
  // `fake_lemond` child doesn't leak (Windows `cargo test` exit hang).
  first.stop(Duration::from_secs(3)).await;
}

/// Delegated-model visibility: a model made resident in the umbrella (its
/// RunningSnapshot persisted, as `start_delegated_lemonade` does) surfaces as
/// its own `status` row under the `lemonade:<name>` launch id, and
/// `stop_model` on that id unloads it from the umbrella + drops the row —
/// while the umbrella itself keeps running.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_projects_delegated_models_and_stop_unloads_them() {
  let logs = unique_temp("delegated");
  std::fs::create_dir_all(&logs).unwrap();
  let registry = SupervisorRegistry::new();
  let port = allocate_port();
  let umbrella = ensure_umbrella(
    &registry,
    port,
    umbrella_spec(port),
    logs.join("lemond.log"),
  )
  .await
  .expect("umbrella spawns");
  wait_ready(&umbrella).await;

  // Share the registry with a MethodContext and persist the snapshot the way
  // `start_delegated_lemonade` does after a successful preload.
  let ctx = MethodContext::new(ShutdownToken::new()).with_supervisors(registry);
  let name = "Qwen2.5-0.5B-Instruct";
  let client = LemonadeClient::new(port).expect("client");
  client.load(name).await.expect("preload");
  let identity = ModelIdentity::Backend(BackendModelId {
    backend: "lemonade".to_string(),
    name: name.to_string(),
  });
  ctx
    .state
    .mutate(|s| {
      s.running.push(RunningSnapshot {
        id: identity,
        pid: 0,
        port,
        started_at: 0,
        params: LaunchParams::new(
          PathBuf::from(format!("lemonade://{name}")),
          LaunchMode::Chat,
        ),
      })
    })
    .await;

  // `status` projects the delegated row: catalog-matching path, the
  // umbrella's port, state mirrored from the (Ready) umbrella.
  let resp = dispatch_request(&ctx, Request::new(1, "status", None)).await;
  let body = resp.result.expect("status result");
  let models = body["models"].as_array().expect("models array");
  let row = models
    .iter()
    .find(|m| m["launch_id"] == format!("lemonade:{name}"))
    .expect("delegated lemonade row must be emitted");
  assert_eq!(row["id"]["path"], format!("lemonade://{name}"));
  assert_eq!(row["port"], json!(port));
  assert_eq!(row["state"]["state"], "ready");
  assert_eq!(row["mode"], "chat");

  // Stop via the delegated id: the umbrella unloads the model (fake_lemond
  // clears its resident slot) and the row disappears; the umbrella row stays.
  let resp = dispatch_request(
    &ctx,
    Request::new(
      2,
      "stop_model",
      Some(json!({"launch_id": format!("lemonade:{name}")})),
    ),
  )
  .await;
  let stop_body = resp.result.expect("stop_model result");
  assert_eq!(stop_body["state"]["state"], "stopped");
  let health = client.health().await.expect("health");
  assert_eq!(
    health.model_loaded, None,
    "stop must unload the model from the umbrella"
  );
  let resp = dispatch_request(&ctx, Request::new(3, "status", None)).await;
  let body = resp.result.expect("status result");
  let models = body["models"].as_array().expect("models array");
  assert!(
    !models
      .iter()
      .any(|m| m["launch_id"] == format!("lemonade:{name}")),
    "stopped delegated row must drop out of status"
  );
  assert!(
    models
      .iter()
      .any(|m| m["launch_id"] == umbrella_launch_id().as_str()),
    "umbrella row must survive a delegated stop"
  );

  // A second stop of the same id is an InvalidParams error (unknown row).
  let resp = dispatch_request(
    &ctx,
    Request::new(
      4,
      "stop_model",
      Some(json!({"launch_id": format!("lemonade:{name}")})),
    ),
  )
  .await;
  assert!(
    resp.error.is_some(),
    "double-stop of a delegated id must error"
  );

  umbrella.stop(Duration::from_secs(3)).await;
}
