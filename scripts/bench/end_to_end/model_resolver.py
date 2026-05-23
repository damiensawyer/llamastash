"""Resolve the four size-class model slots to local GGUF files.

Each slot is configured by an environment variable
(``LLAMASTASH_BENCH_MODELS_{SMALL,MID,LARGE_DENSE,LARGE_MOE}``) whose
value is the absolute path to a local ``.gguf`` file. The harness
does NOT download — staging GGUFs onto the host is the operator's
responsibility (R128's "exact model picks per size class" is
deferred to operator choice via these vars).

SHA-256s are cached at ``~/.cache/llamastash-bench/sha256.json`` keyed
by ``(path, size, mtime_ns)`` so re-runs against the same file don't
re-hash multi-GB blobs.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Optional

from .drivers.base import file_sha256
from .schema import ModelSpec

SLOT_ENV_VARS: dict[str, str] = {
  "small": "LLAMASTASH_BENCH_MODELS_SMALL",
  "mid": "LLAMASTASH_BENCH_MODELS_MID",
  "large_dense": "LLAMASTASH_BENCH_MODELS_LARGE_DENSE",
  "large_moe": "LLAMASTASH_BENCH_MODELS_LARGE_MOE",
}

CACHE_DIR = Path.home() / ".cache" / "llamastash-bench"
CACHE_FILE = CACHE_DIR / "sha256.json"


class ModelSlotMissing(RuntimeError):
  """A requested size_class has no env-var pointing at a local GGUF."""


class ModelFileMissing(FileNotFoundError):
  """The env var is set but the file at that path doesn't exist."""


def _load_cache() -> dict:
  if not CACHE_FILE.exists():
    return {}
  try:
    return json.loads(CACHE_FILE.read_text())
  except (OSError, json.JSONDecodeError):
    return {}


def _save_cache(cache: dict) -> None:
  CACHE_DIR.mkdir(parents=True, exist_ok=True)
  try:
    CACHE_FILE.write_text(json.dumps(cache, indent=2, sort_keys=True))
  except OSError:
    pass


def _cached_sha256(path: Path) -> str:
  stat = path.stat()
  cache = _load_cache()
  key = str(path.resolve())
  entry = cache.get(key)
  if (
    entry
    and entry.get("size") == stat.st_size
    and entry.get("mtime_ns") == stat.st_mtime_ns
  ):
    return entry["sha256"]
  sha = file_sha256(path)
  cache[key] = {
    "size": stat.st_size,
    "mtime_ns": stat.st_mtime_ns,
    "sha256": sha,
  }
  _save_cache(cache)
  return sha


def _split_repo_file(path: Path) -> tuple[str, str]:
  """Best-effort split into (hf-style repo, file). For local files we
  use the parent directory basename as the synthetic repo identifier
  so the schema's hf_repo / hf_file fields stay populated."""
  return (path.parent.name or "local", path.name)


def resolve_slot(size_class: str) -> Optional[ModelSpec]:
  """Return a ModelSpec for `size_class` if the env var is set and
  the file exists. Returns None if the env var is unset (caller
  decides whether that's fatal). Raises ModelFileMissing if the var
  is set but the file is gone."""
  env_var = SLOT_ENV_VARS.get(size_class)
  if env_var is None:
    raise ValueError(f"unknown size_class: {size_class!r}")
  raw = os.environ.get(env_var)
  if not raw:
    return None
  path = Path(raw).expanduser()
  if not path.exists():
    raise ModelFileMissing(
      f"{env_var}={raw} but {path} does not exist; stage the GGUF first"
    )
  if not path.is_file():
    raise ModelFileMissing(f"{env_var} points at a non-file: {path}")
  sha = _cached_sha256(path)
  repo, fname = _split_repo_file(path)
  return ModelSpec(
    size_class=size_class,  # type: ignore[arg-type]
    hf_repo=repo,
    hf_file=fname,
    sha256=sha,
    bytes=path.stat().st_size,
  )


def resolve_slots(size_classes: list[str], require_all: bool = False) -> dict[str, ModelSpec]:
  """Resolve every requested slot. If `require_all`, raises
  ModelSlotMissing for any that don't have an env var set."""
  out: dict[str, ModelSpec] = {}
  missing: list[str] = []
  for cls in size_classes:
    spec = resolve_slot(cls)
    if spec is None:
      missing.append(cls)
      continue
    out[cls] = spec
  if missing and require_all:
    hints = ", ".join(f"{SLOT_ENV_VARS[c]}=<path>" for c in missing)
    raise ModelSlotMissing(f"unset size-class slot(s): {missing} — set {hints}")
  return out


__all__ = [
  "CACHE_FILE",
  "ModelFileMissing",
  "ModelSlotMissing",
  "SLOT_ENV_VARS",
  "resolve_slot",
  "resolve_slots",
]
