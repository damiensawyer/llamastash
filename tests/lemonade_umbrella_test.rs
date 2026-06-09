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

use llamastash::backend::lemonade::{
  ensure_umbrella, umbrella_launch_id, LemonadeBackend, LemonadeClient,
};
use llamastash::backend::{Backend, LaunchPlan};
use llamastash::daemon::probe::ProbeOptions;
use llamastash::daemon::registry::SupervisorRegistry;
use llamastash::daemon::supervisor::{ManagedModel, ManagedState};
use llamastash::launch::mode::LaunchMode;
use llamastash::launch::params::LaunchParams;

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
}
