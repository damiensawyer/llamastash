//! Single-instance enforcement via a PID lockfile.
//!
//! On `acquire`:
//! 1. Try to create `daemon.pid` with `O_CREAT | O_EXCL` and write our PID.
//! 2. If the file already exists, read the PID and probe it with `kill(0)`.
//!    - Alive → return `AlreadyRunning(pid)`. The caller exits zero with a
//!      friendly message.
//!    - Dead (`ESRCH`) → unlink the stale file and retry once.
//!    - Permission denied (`EPERM`) → assume the process is alive but
//!      owned by another user, refuse to take over.
//!
//! On `Drop`, the file is removed. This is best-effort: a process killed
//! with SIGKILL leaves the file behind, which is what the stale-PID
//! detection is for on the next start.

use std::{
  fs::{File, OpenOptions},
  io::{self, Read, Write},
  path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

/// Result of `acquire`.
#[derive(Debug)]
pub enum AcquireOutcome {
  /// We hold the lock. `path` is the file we created; it's removed when
  /// the returned `Lockfile` is dropped.
  Acquired(Lockfile),
  /// Another live process already owns the lock. The caller should exit
  /// gracefully (typically with code 0 and a "daemon already running"
  /// message).
  AlreadyRunning { pid: i32, path: PathBuf },
}

/// Owned lockfile. Removes the file on drop. Held by the daemon for its
/// entire lifetime.
#[derive(Debug)]
pub struct Lockfile {
  path: PathBuf,
  /// Held open so the inode stays pinned for the life of the daemon —
  /// future enhancements can use `flock` on this fd for advisory locking.
  _file: File,
}

impl Lockfile {
  pub fn path(&self) -> &Path {
    &self.path
  }
}

impl Drop for Lockfile {
  fn drop(&mut self) {
    if let Err(e) = std::fs::remove_file(&self.path) {
      if e.kind() != io::ErrorKind::NotFound {
        log::warn!("failed to remove lockfile {}: {e}", self.path.display());
      }
    }
  }
}

/// Errors that prevent `acquire` from reaching a definitive answer. A
/// healthy daemon never returns these.
#[derive(Debug)]
pub enum LockfileError {
  /// State directory was missing or unwritable.
  StateDir(io::Error),
  /// Lockfile content was unreadable or corrupt.
  CorruptLockfile(PathBuf, String),
  /// Filesystem error not covered by the cases above.
  Io(io::Error),
}

impl std::fmt::Display for LockfileError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::StateDir(e) => write!(f, "could not prepare state dir: {e}"),
      Self::CorruptLockfile(p, msg) => {
        write!(
          f,
          "lockfile {} is corrupt ({msg}); remove it and retry",
          p.display()
        )
      }
      Self::Io(e) => write!(f, "lockfile i/o: {e}"),
    }
  }
}

impl std::error::Error for LockfileError {}

impl From<io::Error> for LockfileError {
  fn from(e: io::Error) -> Self {
    Self::Io(e)
  }
}

/// Try to acquire the PID lockfile at `state_dir/daemon.pid`. Creates
/// `state_dir` if it doesn't exist. See module docs for the policy on
/// stale PIDs.
pub fn acquire(state_dir: &Path) -> Result<AcquireOutcome, LockfileError> {
  std::fs::create_dir_all(state_dir).map_err(LockfileError::StateDir)?;
  let path = state_dir.join("daemon.pid");
  // Two-pass: first attempt creates a fresh file. If it fails with
  // AlreadyExists, we probe the PID and either bail (live) or unlink and
  // retry (stale).
  for attempt in 0..2 {
    match try_create_pidfile(&path) {
      Ok(file) => return Ok(AcquireOutcome::Acquired(Lockfile { path, _file: file })),
      Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
        let pid = read_pid(&path)?;
        if pid_is_alive(pid) {
          return Ok(AcquireOutcome::AlreadyRunning { pid, path });
        }
        // Stale — drop it and loop. Cap attempts so a permanent
        // permission error or another racing daemon don't trap us.
        if attempt == 0 {
          log::info!(
            "removing stale lockfile {} (pid {pid} not alive)",
            path.display()
          );
          std::fs::remove_file(&path)?;
          continue;
        }
        return Err(LockfileError::Io(io::Error::other(
          "lockfile reappeared after stale-pid cleanup; another daemon raced us",
        )));
      }
      Err(e) => return Err(LockfileError::Io(e)),
    }
  }
  unreachable!("loop exits via return on every path")
}

fn try_create_pidfile(path: &Path) -> io::Result<File> {
  let mut opts = OpenOptions::new();
  opts.write(true).create_new(true);
  #[cfg(unix)]
  {
    opts.mode(0o600);
  }
  let mut file = opts.open(path)?;
  writeln!(file, "{}", std::process::id())?;
  file.sync_all()?;
  Ok(file)
}

fn read_pid(path: &Path) -> Result<i32, LockfileError> {
  let mut contents = String::new();
  File::open(path)?.read_to_string(&mut contents)?;
  contents
    .trim()
    .parse::<i32>()
    .map_err(|e| LockfileError::CorruptLockfile(path.to_path_buf(), e.to_string()))
}

/// Is `pid` a live process? Uses `kill(pid, 0)` — a signal value of 0
/// performs the existence check without actually delivering a signal.
/// `EPERM` indicates the process exists but is owned by another UID; we
/// treat that as "alive" so a daemon under a different user isn't kicked
/// out.
#[cfg(unix)]
fn pid_is_alive(pid: i32) -> bool {
  if pid <= 0 {
    return false;
  }
  // SAFETY: `kill(2)` with signal 0 is the documented existence check;
  // no memory is touched.
  let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
  if ret == 0 {
    return true;
  }
  // `errno` access path
  let err = io::Error::last_os_error();
  match err.raw_os_error() {
    Some(libc::ESRCH) => false, // no such process
    Some(libc::EPERM) => true,  // exists, owned by another uid
    _ => {
      log::warn!("kill(pid={pid}, 0) returned unexpected error: {err}; assuming alive");
      true
    }
  }
}

#[cfg(not(unix))]
fn pid_is_alive(_pid: i32) -> bool {
  // Non-Unix isn't a supported target for the daemon; conservatively say
  // yes so we never silently steal a peer's lockfile.
  true
}

#[cfg(test)]
mod tests {
  use std::time::{SystemTime, UNIX_EPOCH};

  use super::*;

  fn temp_state_dir(name: &str) -> PathBuf {
    let suffix = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .expect("clock should be after epoch")
      .as_nanos();
    let dir = std::env::temp_dir().join(format!(
      "llamatui-lockfile-{name}-{}-{suffix}",
      std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir should be created");
    dir
  }

  #[test]
  fn acquire_creates_pidfile_when_absent() {
    let dir = temp_state_dir("fresh");
    let outcome = acquire(&dir).expect("acquire should succeed");
    match outcome {
      AcquireOutcome::Acquired(lock) => {
        let raw = std::fs::read_to_string(lock.path()).expect("pidfile readable");
        assert_eq!(raw.trim(), std::process::id().to_string());
      }
      AcquireOutcome::AlreadyRunning { pid, .. } => panic!("unexpected AlreadyRunning(pid={pid})"),
    }
    std::fs::remove_dir_all(&dir).ok();
  }

  #[test]
  fn drop_removes_pidfile() {
    let dir = temp_state_dir("drop");
    let path = {
      let lock = match acquire(&dir).expect("acquire") {
        AcquireOutcome::Acquired(l) => l,
        AcquireOutcome::AlreadyRunning { .. } => panic!("unexpected AlreadyRunning"),
      };
      let p = lock.path().to_path_buf();
      drop(lock);
      p
    };
    assert!(
      !path.exists(),
      "drop must remove the pidfile, still at {}",
      path.display()
    );
    std::fs::remove_dir_all(&dir).ok();
  }

  #[test]
  fn second_acquire_with_live_pid_reports_already_running() {
    let dir = temp_state_dir("live");
    let _first = match acquire(&dir).expect("first acquire") {
      AcquireOutcome::Acquired(l) => l,
      AcquireOutcome::AlreadyRunning { .. } => panic!("unexpected AlreadyRunning"),
    };
    let outcome = acquire(&dir).expect("second acquire");
    match outcome {
      AcquireOutcome::AlreadyRunning { pid, .. } => {
        assert_eq!(pid, std::process::id() as i32);
      }
      AcquireOutcome::Acquired(_) => panic!("second acquire should observe live pid"),
    }
    drop(_first);
    std::fs::remove_dir_all(&dir).ok();
  }

  #[test]
  fn stale_pidfile_is_removed_and_re_acquired() {
    let dir = temp_state_dir("stale");
    let path = dir.join("daemon.pid");
    // PID 1 sometimes exists, so use a value that's almost certainly free:
    // 2^31 - 1 is the kernel max on 64-bit Linux and an unallocated value.
    std::fs::write(&path, "2147483646\n").expect("seed stale pidfile");
    let outcome = acquire(&dir).expect("acquire");
    match outcome {
      AcquireOutcome::Acquired(lock) => {
        let raw = std::fs::read_to_string(lock.path()).expect("readable");
        assert_eq!(raw.trim(), std::process::id().to_string());
      }
      AcquireOutcome::AlreadyRunning { pid, .. } => {
        panic!("stale pid {pid} should have been cleaned up")
      }
    }
    std::fs::remove_dir_all(&dir).ok();
  }

  #[test]
  fn corrupt_pidfile_surfaces_actionable_error() {
    let dir = temp_state_dir("corrupt");
    let path = dir.join("daemon.pid");
    std::fs::write(&path, "this is not a pid").expect("seed corrupt pidfile");
    let err = acquire(&dir).expect_err("corrupt lockfile should error");
    match err {
      LockfileError::CorruptLockfile(p, _) => assert_eq!(p, path),
      other => panic!("expected CorruptLockfile, got {other:?}"),
    }
    std::fs::remove_dir_all(&dir).ok();
  }

  #[cfg(unix)]
  #[test]
  fn pidfile_mode_is_0600_on_unix() {
    use std::os::unix::fs::PermissionsExt;

    let dir = temp_state_dir("perms");
    let lock = match acquire(&dir).expect("acquire") {
      AcquireOutcome::Acquired(l) => l,
      AcquireOutcome::AlreadyRunning { .. } => panic!("unexpected AlreadyRunning"),
    };
    let mode = std::fs::metadata(lock.path())
      .expect("metadata")
      .permissions()
      .mode()
      & 0o777;
    assert_eq!(mode, 0o600, "pidfile must be 0600");
    drop(lock);
    std::fs::remove_dir_all(&dir).ok();
  }

  #[test]
  fn pid_is_alive_returns_false_for_obviously_dead_pid() {
    assert!(!pid_is_alive(0));
    assert!(!pid_is_alive(-1));
  }

  #[test]
  fn pid_is_alive_returns_true_for_self() {
    assert!(pid_is_alive(std::process::id() as i32));
  }
}
