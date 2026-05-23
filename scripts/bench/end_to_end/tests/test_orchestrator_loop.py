"""Unit tests for the wired Suite B orchestrator loop.

The driver layer is patched with a stub so we exercise the loop
without spawning real tools. Real cell execution lands in Unit 8 and
needs a real GPU + a real GGUF.

Coverage:
- model-slot env-var resolution (path, repo synthesis, sha cache key)
- _knobs_for_workload special-cases rag_prefill ctx
- _unfair_knobs_for_driver subtracts driver-supported from requested
- _run_cell records reps + summary + determinism + unfair when happy path
- _run_cell records `driver-error: ...` notes when start() raises
- _execute_matrix continues past a single bad cell
"""
from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Optional
from unittest.mock import patch

import pytest

from scripts.bench.end_to_end import model_resolver, orchestrator
from scripts.bench.end_to_end.drivers.base import (
  Driver,
  DriverError,
  Mode,
  ModelHandle,
  NormalizedKnobs,
)
from scripts.bench.end_to_end.schema import ModelSpec
from scripts.bench.end_to_end.workloads import WorkloadResult


# ---- model_resolver ---------------------------------------------------


def test_resolve_slot_returns_none_when_unset(monkeypatch: pytest.MonkeyPatch) -> None:
  for var in model_resolver.SLOT_ENV_VARS.values():
    monkeypatch.delenv(var, raising=False)
  assert model_resolver.resolve_slot("small") is None


def test_resolve_slot_raises_on_missing_file(
  monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
  monkeypatch.setenv("LLAMASTASH_BENCH_MODELS_SMALL", str(tmp_path / "missing.gguf"))
  with pytest.raises(model_resolver.ModelFileMissing):
    model_resolver.resolve_slot("small")


def test_resolve_slot_builds_modelspec(
  monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
  parent = tmp_path / "MyOrg" / "Model-GGUF"
  parent.mkdir(parents=True)
  gguf = parent / "model-q4.gguf"
  gguf.write_bytes(b"GGUF" + b"\x00" * 64)
  # Redirect SHA cache so it doesn't pollute the user's HOME.
  monkeypatch.setattr(model_resolver, "CACHE_FILE", tmp_path / "sha.json")
  monkeypatch.setattr(model_resolver, "CACHE_DIR", tmp_path)
  monkeypatch.setenv("LLAMASTASH_BENCH_MODELS_SMALL", str(gguf))

  spec = model_resolver.resolve_slot("small")
  assert spec is not None
  assert spec.size_class == "small"
  assert spec.hf_repo == "Model-GGUF"
  assert spec.hf_file == "model-q4.gguf"
  assert len(spec.sha256) == 64
  assert spec.bytes == 68


def test_resolve_slots_require_all_raises_with_hints(
  monkeypatch: pytest.MonkeyPatch,
) -> None:
  for var in model_resolver.SLOT_ENV_VARS.values():
    monkeypatch.delenv(var, raising=False)
  with pytest.raises(model_resolver.ModelSlotMissing) as exc:
    model_resolver.resolve_slots(["small", "mid"], require_all=True)
  assert "LLAMASTASH_BENCH_MODELS_SMALL" in str(exc.value)
  assert "LLAMASTASH_BENCH_MODELS_MID" in str(exc.value)


# ---- _knobs_for_workload + _unfair_knobs_for_driver -----------------


def test_knobs_for_workload_rag_prefill_overrides_ctx() -> None:
  knobs = orchestrator._knobs_for_workload("rag_prefill")
  assert knobs.ctx == orchestrator.RAG_PREFILL_CTX


def test_knobs_for_workload_uses_base_otherwise() -> None:
  for wl in ["chat_turn", "agent_decode", "parallel_4"]:
    knobs = orchestrator._knobs_for_workload(wl)
    assert knobs.ctx == orchestrator.BASE_NORMALIZED_KNOBS.ctx


class _FakeSupports(Driver):
  name = "fake"

  def __init__(self, supports: set[str]) -> None:
    self._supports = supports

  def normalized_knobs_supported(self) -> set[str]:
    return self._supports

  def version_string(self) -> Optional[str]:  # pragma: no cover - protocol
    return None

  def prepare_model(self, gguf_path: Path, mode: Mode) -> ModelHandle:  # pragma: no cover
    return ModelHandle(name="fake", source_path=gguf_path)

  def start(
    self, handle: ModelHandle, mode: Mode, knobs: Optional[NormalizedKnobs] = None
  ) -> str:  # pragma: no cover
    return "http://127.0.0.1:0"

  def stop(self) -> None:  # pragma: no cover
    pass

  def recorded_argv(self) -> list[str]:  # pragma: no cover
    return []


def test_unfair_knobs_subtracts_supported() -> None:
  applied = NormalizedKnobs(
    ctx=4096, n_gpu_layers=999, flash_attn=True, kv_cache_type="f16",
    batch_size=512, ubatch_size=512,
  )
  fake = _FakeSupports({"ctx", "n_gpu_layers"})
  unfair = orchestrator._unfair_knobs_for_driver(fake, applied)
  assert "ctx" not in unfair
  assert "flash_attn" in unfair
  assert "batch_size" in unfair


# ---- _run_cell happy path + error path -----------------------------


class _StubDriver:
  """Stand-in driver that returns canned WorkloadResults via the
  module-level `_STUB_RESULTS` list (one per rep). `start` raises if
  `_STUB_RAISES_ON_START` is set."""

  name = "stub"
  _STUB_RESULTS: list[WorkloadResult] = []
  _STUB_RAISES_ON_START = False
  _STUB_ARGV: list[str] = ["stub", "--port", "18000", "-m", "/tmp/x.gguf"]

  def __init__(self) -> None:
    self._handle: Optional[ModelHandle] = None

  def version_string(self) -> Optional[str]:
    return "stub-1.0"

  def prepare_model(self, gguf_path: Path, mode: Mode) -> ModelHandle:
    return ModelHandle(name=str(gguf_path), source_path=gguf_path)

  def start(self, handle: ModelHandle, mode: Mode, knobs=None) -> str:
    if self._STUB_RAISES_ON_START:
      raise DriverError("stub-start-failed")
    self._handle = handle
    return "http://127.0.0.1:18000"

  def stop(self) -> None:
    self._handle = None

  def normalized_knobs_supported(self) -> set[str]:
    return {"ctx", "n_gpu_layers", "flash_attn", "kv_cache_type", "batch_size", "ubatch_size"}

  def recorded_argv(self) -> list[str]:
    return [a for a in self._STUB_ARGV if a not in {"--port", "18000"}]


def _make_spec(tmp_path: Path) -> ModelSpec:
  return ModelSpec(
    size_class="small",
    hf_repo="org",
    hf_file="m.gguf",
    sha256="a" * 64,
    bytes=1024,
  )


def _stub_make_driver(_name: str) -> Driver:  # type: ignore[type-var]
  return _StubDriver()


def _drain_results_in_order(monkeypatch: pytest.MonkeyPatch) -> None:
  """run_workload pops one canned result per call."""
  async def _take(name, base_url, model, rep_index, is_warmup=False, client=None):
    return _StubDriver._STUB_RESULTS.pop(0)
  monkeypatch.setattr(orchestrator, "run_workload", _take)


def test_run_cell_happy_path(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
  monkeypatch.setattr(orchestrator, "make_driver", _stub_make_driver)
  monkeypatch.setenv("LLAMASTASH_BENCH_MODELS_SMALL", str(tmp_path / "small.gguf"))
  (tmp_path / "small.gguf").write_bytes(b"x")

  _StubDriver._STUB_RAISES_ON_START = False
  _StubDriver._STUB_RESULTS = [
    WorkloadResult(rep_index=0, is_warmup=True, prompt_text="p", output_text="o0",
                   ttft_ms=10.0, decode_tps=20.0, decode_tokens=5, e2e_latency_ms=100.0),
    WorkloadResult(rep_index=1, is_warmup=False, prompt_text="p", output_text="o1",
                   ttft_ms=12.0, decode_tps=22.0, decode_tokens=5, e2e_latency_ms=110.0),
    WorkloadResult(rep_index=2, is_warmup=False, prompt_text="p", output_text="o2",
                   ttft_ms=11.0, decode_tps=21.0, decode_tokens=5, e2e_latency_ms=105.0),
  ]
  _drain_results_in_order(monkeypatch)

  spec = _make_spec(tmp_path)
  cell = asyncio.run(
    orchestrator._run_cell(
      tool="llamacpp", model_spec=spec, mode_str="normalized",
      workload="chat_turn", reps=3,
    )
  )
  assert cell.notes == ""
  assert cell.summary.measured_rep_count == 2
  assert cell.summary.decode_tps_mean is not None
  assert "--port" not in cell.argv_recorded
  assert cell.determinism.prompt_sha256 is not None
  # Cell's tool stays the requested one, regardless of the stub registry hijack.
  assert cell.tool == "llamacpp"


def test_run_cell_driver_error_records_notes(
  monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
  monkeypatch.setattr(orchestrator, "make_driver", _stub_make_driver)
  monkeypatch.setenv("LLAMASTASH_BENCH_MODELS_SMALL", str(tmp_path / "small.gguf"))
  (tmp_path / "small.gguf").write_bytes(b"x")

  _StubDriver._STUB_RAISES_ON_START = True
  _StubDriver._STUB_RESULTS = []

  spec = _make_spec(tmp_path)
  cell = asyncio.run(
    orchestrator._run_cell(
      tool="llamacpp", model_spec=spec, mode_str="defaults",
      workload="chat_turn", reps=2,
    )
  )
  assert "driver-error" in cell.notes
  assert cell.summary.measured_rep_count == 0
  _StubDriver._STUB_RAISES_ON_START = False


# ---- _execute_matrix continues past errors -------------------------


def test_execute_matrix_skips_failing_cell_and_continues(
  monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
  import argparse

  monkeypatch.setattr(orchestrator, "make_driver", _stub_make_driver)
  monkeypatch.setenv("LLAMASTASH_BENCH_MODELS_SMALL", str(tmp_path / "small.gguf"))
  (tmp_path / "small.gguf").write_bytes(b"x")

  call_count = {"n": 0}

  async def fake_run_cell(tool, model_spec, mode_str, workload, reps):
    call_count["n"] += 1
    if call_count["n"] == 1:
      raise RuntimeError("first-cell-blows-up")
    return await _real_run_cell_inert(tool, model_spec, mode_str, workload, reps)

  async def _real_run_cell_inert(tool, spec, mode, wl, reps):
    from scripts.bench.end_to_end.schema import Cell, Summary
    return Cell(
      tool=tool, model=spec, mode=mode, workload=wl,
      summary=Summary(measured_rep_count=1, decode_tps_mean=10.0, ttft_ms_mean=10.0),
    )

  monkeypatch.setattr(orchestrator, "_run_cell", fake_run_cell)

  args = argparse.Namespace(
    tools=["llamacpp", "llamastash"], models=["small"], modes=["defaults"],
    workloads=["chat_turn"], reps=1, continue_on_cell_error=True,
  )
  spec = _make_spec(tmp_path)
  cells = asyncio.run(orchestrator._execute_matrix(args, {"small": spec}))
  assert len(cells) == 1  # the second cell ran; the first was skipped
  assert call_count["n"] == 2
