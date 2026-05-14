//! Daemon-process lifecycle tests: lockfile contention, stale-lock
//! recovery, and the cleanup invariant that shutdown removes both the
//! socket and the pidfile.

use std::{
  path::{Path, PathBuf},
  time::Duration,
};

use llamatui::daemon::{run_foreground, DaemonOptions, StartOutcome};
use llamatui::ipc::Client;
use tokio::time::timeout;

fn unique_temp_dir(label: &str) -> PathBuf {
  let suffix = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .expect("clock")
    .as_nanos();
  let dir = std::env::temp_dir().join(format!(
    "llamatui-lifecycle-{label}-{}-{suffix}",
    std::process::id()
  ));
  std::fs::create_dir_all(&dir).expect("temp dir creation");
  dir
}

fn opts_for(temp: &Path) -> DaemonOptions {
  DaemonOptions {
    state_dir: temp.to_path_buf(),
    socket_path: temp.join("daemon.sock"),
  }
}

/// Poll until a connection to `path` succeeds — file existence isn't
/// enough because the test fixture can pre-seed a regular file at the
/// same path; the daemon will remove it and re-bind.
async fn wait_for_socket(path: &Path) {
  let deadline = std::time::Instant::now() + Duration::from_secs(3);
  loop {
    if std::time::Instant::now() > deadline {
      panic!(
        "daemon did not become connectable within 3s: {}",
        path.display()
      );
    }
    if Client::connect(path).await.is_ok() {
      return;
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_start_reports_already_running() {
  let dir = unique_temp_dir("dup");
  let opts = opts_for(&dir);
  let opts_copy = opts.clone();

  let handle = tokio::spawn(async move { run_foreground(opts).await });
  wait_for_socket(&opts_copy.socket_path).await;

  // Same state_dir — should observe the live pidfile and bail out.
  let outcome = run_foreground(opts_copy.clone())
    .await
    .expect("second start should return Ok");
  match outcome {
    StartOutcome::AlreadyRunning(pid) => assert_eq!(pid, std::process::id() as i32),
    StartOutcome::RanToCompletion => panic!("second start should not take the lock"),
  }

  // Shutdown the first daemon so the test cleans up.
  let mut client = Client::connect(&opts_copy.socket_path)
    .await
    .expect("connect to first daemon");
  let _ = client.call("shutdown", None).await.expect("shutdown");
  timeout(Duration::from_secs(3), handle)
    .await
    .expect("first daemon must exit")
    .expect("join")
    .expect("daemon result");

  std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_removes_socket_and_pidfile() {
  let dir = unique_temp_dir("cleanup");
  let opts = opts_for(&dir);
  let socket = opts.socket_path.clone();
  let pidfile = dir.join("daemon.pid");
  let handle = tokio::spawn(async move { run_foreground(opts).await });
  wait_for_socket(&socket).await;

  assert!(pidfile.exists(), "pidfile must exist while daemon runs");

  let mut client = Client::connect(&socket).await.expect("connect");
  let _ = client.call("shutdown", None).await.expect("shutdown");
  timeout(Duration::from_secs(3), handle)
    .await
    .expect("daemon must exit")
    .expect("join")
    .expect("daemon result");

  assert!(!socket.exists(), "socket file must be removed on shutdown");
  assert!(!pidfile.exists(), "pidfile must be removed on shutdown");

  std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_socket_is_cleaned_before_bind() {
  let dir = unique_temp_dir("stale-sock");
  let opts = opts_for(&dir);
  let socket = opts.socket_path.clone();

  // Drop a non-socket file at the socket path to simulate a SIGKILL'd
  // previous run that never got to clean up. The daemon must remove it
  // before binding rather than failing.
  std::fs::write(&socket, b"this used to be a socket").expect("seed stale file");

  let handle = tokio::spawn(async move { run_foreground(opts).await });
  wait_for_socket(&socket).await;

  // Confirm we're talking to a real listener, not the stale file.
  let mut client = Client::connect(&socket).await.expect("connect");
  let _ = client
    .call("ping", None)
    .await
    .expect("ping after stale cleanup");
  let _ = client.call("shutdown", None).await.expect("shutdown");
  timeout(Duration::from_secs(3), handle)
    .await
    .expect("daemon exits")
    .expect("join")
    .expect("daemon result");

  std::fs::remove_dir_all(&dir).ok();
}
