//! Favorited models (storage half; TUI mark/render lives in the TUI).
//!
//! Persisted as `favorites: Vec<FavoriteEntry>` in `state.json`. The
//! set is small (humans favorite tens of models, not thousands) so a
//! `Vec` keyed by `ModelId` is cheaper than a `BTreeSet` once you
//! account for the extra `Ord` derive.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::backend::identity::ModelIdentity;

/// One favorited model. Lean wrapper so future fields (a colour, a
/// reminder note, a pinned preset) can land without breaking the
/// `state.json` schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FavoriteEntry {
  pub id: ModelIdentity,
}

/// In-memory favourites set with stable iteration order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Favorites {
  entries: Vec<FavoriteEntry>,
}

impl Favorites {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn len(&self) -> usize {
    self.entries.len()
  }

  pub fn is_empty(&self) -> bool {
    self.entries.is_empty()
  }

  pub fn iter(&self) -> std::slice::Iter<'_, FavoriteEntry> {
    self.entries.iter()
  }

  /// Returns true if `id` was added (false if already present). Used
  /// by the IPC `favorite_add` method so the response can distinguish
  /// no-op from new-add.
  pub fn add(&mut self, id: ModelIdentity) -> bool {
    if self.entries.iter().any(|e| e.id == id) {
      return false;
    }
    self.entries.push(FavoriteEntry { id });
    true
  }

  /// Returns true if `id` was removed.
  pub fn remove(&mut self, id: &ModelIdentity) -> bool {
    let before = self.entries.len();
    self.entries.retain(|e| &e.id != id);
    self.entries.len() != before
  }

  pub fn contains(&self, id: &ModelIdentity) -> bool {
    self.entries.iter().any(|e| &e.id == id)
  }

  /// Set view of the contained identities, useful for diffing two
  /// favorite sets.
  pub fn as_set(&self) -> BTreeSet<&ModelIdentity> {
    self.entries.iter().map(|e| &e.id).collect()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  use std::path::PathBuf;

  fn id(path: &str, tag: u8) -> ModelIdentity {
    ModelIdentity::Gguf(crate::gguf::identity::ModelId {
      path: PathBuf::from(path),
      header_blake3: [tag; 32],
    })
  }

  #[test]
  fn add_idempotent_returns_false_on_second_call() {
    let mut f = Favorites::new();
    assert!(f.add(id("/m/a.gguf", 1)));
    assert!(!f.add(id("/m/a.gguf", 1)));
    assert_eq!(f.len(), 1);
  }

  #[test]
  fn remove_returns_true_only_when_present() {
    let mut f = Favorites::new();
    f.add(id("/m/a.gguf", 1));
    assert!(f.remove(&id("/m/a.gguf", 1)));
    assert!(!f.remove(&id("/m/a.gguf", 1)));
  }

  #[test]
  fn iter_order_is_insertion_order() {
    let mut f = Favorites::new();
    f.add(id("/b.gguf", 2));
    f.add(id("/a.gguf", 1));
    let paths: Vec<_> = f
      .iter()
      .map(|e| e.id.as_gguf().unwrap().path.clone())
      .collect();
    assert_eq!(
      paths,
      vec![PathBuf::from("/b.gguf"), PathBuf::from("/a.gguf")]
    );
  }

  #[test]
  fn json_round_trip() {
    let mut f = Favorites::new();
    f.add(id("/m/a.gguf", 1));
    f.add(id("/m/b.gguf", 2));
    let v = serde_json::to_string(&f).unwrap();
    let back: Favorites = serde_json::from_str(&v).unwrap();
    assert_eq!(back, f);
  }
}
