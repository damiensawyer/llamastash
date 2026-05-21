//! Single-flight launch coalescing keyed on [`ModelId`].
//!
//! Auto-start (Unit 4) can fire from any number of concurrent inbound
//! requests for the same dormant model. Without coalescing, each
//! request would issue its own `start_model_inner` call, race the
//! port allocator, and either fail with a port collision or burn a
//! second `llama-server` process on top of the one already launching.
//!
//! This module hands the proxy a tiny `Map<ModelId, Arc<Notify>>`.
//! The first caller for a given model inserts a fresh
//! [`tokio::sync::Notify`], runs the launch, and signals waiters on
//! completion (Ready *or* Error). Concurrent callers find the
//! existing slot and `.notified().await` instead of issuing their
//! own launch. Once everyone is awake, each caller re-snapshots the
//! supervisor registry and proceeds: happy waiters forward against
//! the now-Ready model, the few that observed `Error{cause}` enter
//! the fallback path independently.
//!
//! Keyed on [`ModelId`] — the canonical `(path, header_blake3)` pair
//! — rather than the raw `body.model` string, so two requests with
//! different fuzzy spellings of the same model still share one
//! launch.
//!
//! `Mutex<HashMap<…>>` over `RwLock<HashMap<…>>`: every interesting
//! operation is a write (insert / remove / "do we already have an
//! Arc?"). The contention is low — auto-start runs at human-typing
//! cadence — but a read lock would force the lookup case to
//! upgrade, which `RwLock` doesn't model on tokio. Tokio's
//! [`tokio::sync::Mutex`] gives us straightforward "lock → check →
//! mutate → drop" semantics on an awaitable guard.
//!
//! Plan: docs/plans/2026-05-21-001-feat-proxy-router-plan.md (Unit 4).

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{Mutex, Notify};

use crate::gguf::identity::ModelId;

/// Single-flight registry. Cheap to clone — every field lives behind
/// an `Arc` so the per-request handle is a refcount bump.
#[derive(Clone, Default)]
pub(crate) struct Coalesce {
  inner: Arc<Mutex<HashMap<ModelId, Arc<Notify>>>>,
}

/// Outcome of [`Coalesce::acquire`]. Tells the caller whether to
/// drive the launch itself (the [`Leader`]) or to park on an
/// existing waiter and re-check state when notified ([`Follower`]).
pub(crate) enum AcquireOutcome {
  /// This caller is the first to ask for the launch. It owns the
  /// `Notify` until it calls [`Leader::finish`].
  Leader(Leader),
  /// Another caller is already driving the launch. The follower
  /// awaits the [`Notify`] and re-snapshots the supervisor map.
  Follower(Follower),
}

/// Token returned to the request that won the right to drive the
/// launch. Holding this token is the marker that this request's
/// `start_model_inner` call is the live one. Dropping it without
/// calling [`Leader::finish`] is a bug — followers would park
/// forever — but a guard `Drop` impl makes the worst case "everyone
/// wakes up early and retries" rather than "everyone hangs."
pub(crate) struct Leader {
  parent: Coalesce,
  key: ModelId,
  notify: Arc<Notify>,
  /// Becomes `true` after [`Self::finish`] runs. The `Drop` impl
  /// uses it to detect the "leader dropped without calling finish"
  /// failure mode and signal waiters anyway.
  finished: bool,
}

impl Leader {
  /// Notify every parked follower that the launch attempt has
  /// concluded (whether Ready, Error, or anything else). Removes the
  /// `Notify` from the map so the next request for the same model
  /// starts fresh.
  pub(crate) async fn finish(mut self) {
    self.complete().await;
  }

  /// Internal: shared body of `finish` + the `Drop` safety net.
  async fn complete(&mut self) {
    if self.finished {
      return;
    }
    self.finished = true;
    // Drop the map entry first so a follower wake-up which raced
    // ahead and re-queried `acquire` sees an empty slot (and so
    // becomes the next leader if the launch failed).
    self.parent.inner.lock().await.remove(&self.key);
    self.notify.notify_waiters();
  }
}

impl Drop for Leader {
  fn drop(&mut self) {
    if self.finished {
      return;
    }
    // We can't await in Drop. Fire-and-forget the wake-up on the
    // tokio runtime so followers don't hang forever if the leader's
    // future is cancelled or panics mid-launch. This is best-effort:
    // if no runtime is available we leak the entry, which the next
    // proxy request to acquire the same key will overwrite.
    let parent = self.parent.clone();
    let key = self.key.clone();
    let notify = self.notify.clone();
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
      handle.spawn(async move {
        parent.inner.lock().await.remove(&key);
        notify.notify_waiters();
      });
    }
  }
}

/// Token returned to followers that arrived while a launch was
/// already in flight. The caller awaits [`Self::wait`] then
/// re-snapshots the supervisor registry to find out whether the
/// launch reached Ready (forward) or `Error{cause}` (fall through
/// to fallback).
pub(crate) struct Follower {
  notify: Arc<Notify>,
}

impl Follower {
  /// Block until the leader signals completion. Returns once the
  /// leader has called [`Leader::finish`] (or dropped without
  /// calling it, via the safety-net `Drop` impl).
  pub(crate) async fn wait(self) {
    self.notify.notified().await;
  }
}

impl Coalesce {
  pub(crate) fn new() -> Self {
    Self::default()
  }

  /// Try to acquire single-flight rights for `key`. Either a fresh
  /// [`Leader`] or an existing [`Follower`] is returned; the caller
  /// branches on the variant.
  ///
  /// The lookup-and-insert happens under one lock, so two concurrent
  /// `acquire(key)` calls can never both walk away as leaders for
  /// the same `key`.
  pub(crate) async fn acquire(&self, key: ModelId) -> AcquireOutcome {
    let mut guard = self.inner.lock().await;
    if let Some(notify) = guard.get(&key).cloned() {
      return AcquireOutcome::Follower(Follower { notify });
    }
    let notify = Arc::new(Notify::new());
    guard.insert(key.clone(), notify.clone());
    AcquireOutcome::Leader(Leader {
      parent: self.clone(),
      key,
      notify,
      finished: false,
    })
  }
}

#[cfg(test)]
mod tests {
  use std::path::PathBuf;
  use std::sync::atomic::{AtomicUsize, Ordering};
  use std::time::Duration;

  use super::*;

  fn key(path: &str) -> ModelId {
    ModelId {
      path: PathBuf::from(path),
      header_blake3: [1u8; 32],
    }
  }

  #[tokio::test]
  async fn first_caller_becomes_leader() {
    let c = Coalesce::new();
    let outcome = c.acquire(key("/m/a.gguf")).await;
    assert!(matches!(outcome, AcquireOutcome::Leader(_)));
  }

  #[tokio::test]
  async fn second_caller_becomes_follower() {
    let c = Coalesce::new();
    let leader = match c.acquire(key("/m/a.gguf")).await {
      AcquireOutcome::Leader(l) => l,
      _ => panic!("expected leader"),
    };
    let outcome = c.acquire(key("/m/a.gguf")).await;
    assert!(matches!(outcome, AcquireOutcome::Follower(_)));
    leader.finish().await;
  }

  #[tokio::test]
  async fn different_keys_each_get_a_leader() {
    let c = Coalesce::new();
    let a = c.acquire(key("/m/a.gguf")).await;
    let b = c.acquire(key("/m/b.gguf")).await;
    assert!(matches!(a, AcquireOutcome::Leader(_)));
    assert!(matches!(b, AcquireOutcome::Leader(_)));
  }

  #[tokio::test]
  async fn follower_wakes_when_leader_finishes() {
    let c = Coalesce::new();
    let leader = match c.acquire(key("/m/a.gguf")).await {
      AcquireOutcome::Leader(l) => l,
      _ => panic!("leader"),
    };
    let follower = match c.acquire(key("/m/a.gguf")).await {
      AcquireOutcome::Follower(f) => f,
      _ => panic!("follower"),
    };
    let woke = Arc::new(AtomicUsize::new(0));
    let woke_for_task = woke.clone();
    let task = tokio::spawn(async move {
      follower.wait().await;
      woke_for_task.fetch_add(1, Ordering::SeqCst);
    });
    // Yield to let the follower start awaiting before we fire
    // notify_waiters; `Notify::notify_waiters` only wakes parked
    // waiters, so the follower needs to be parked already.
    tokio::time::sleep(Duration::from_millis(20)).await;
    leader.finish().await;
    task.await.unwrap();
    assert_eq!(woke.load(Ordering::SeqCst), 1);
  }

  #[tokio::test]
  async fn finish_clears_the_slot_so_next_acquire_is_a_fresh_leader() {
    let c = Coalesce::new();
    let leader = match c.acquire(key("/m/a.gguf")).await {
      AcquireOutcome::Leader(l) => l,
      _ => panic!("leader"),
    };
    leader.finish().await;
    let again = c.acquire(key("/m/a.gguf")).await;
    assert!(matches!(again, AcquireOutcome::Leader(_)));
  }
}
