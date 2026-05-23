"""Suite B (cross-tool end-to-end comparison) orchestrator.

Walks the full matrix:

    backends (auto-detected, single per host)
      × models   {small, mid, large_dense, [large_moe]}
      × tools    {llamastash, llamacpp, ollama, lmstudio}
      × modes    {defaults, normalized}
      × workloads (4)
      × reps     (1 warmup + N measured)

Per cell: instantiate the driver, prepare the model, start the
server, run `reps` workload invocations on the same event loop
(rep 0 is warmup, excluded from `Summary`), stop the driver, and
record an aggregated `Cell` on the `RunReport`. A failed cell is
recorded with `notes` describing the error and the matrix continues.

Output JSON lands at
``docs/benchmarks/runs/<host-id>/<DATE>-<sha>.json``; pass
``--render`` to also run the renderer afterwards so the dated
markdown page picks up the new file.
"""
from __future__ import annotations

import argparse
import asyncio
import datetime as dt
import os
import subprocess
import sys
from pathlib import Path
from typing import Optional

import httpx

from .drivers import make_driver
from .drivers.base import Driver, DriverError, Mode, NormalizedKnobs
from .metrics import FairnessSample, fairness_check, summarize
from .model_resolver import (
  SLOT_ENV_VARS,
  ModelFileMissing,
  ModelSlotMissing,
  resolve_slots,
)
from .provenance import capture_host, capture_provenance
from .schema import Cell, Determinism, ModelSpec, Rep, RunReport
from .workloads import WorkloadResult, run_workload

DEFAULT_TOOLS = ["llamastash", "llamacpp", "ollama", "lmstudio"]
DEFAULT_MODELS = ["small", "mid", "large_dense"]
DEFAULT_MODES = ["defaults", "normalized"]
DEFAULT_WORKLOADS = ["chat_turn", "rag_prefill", "agent_decode", "parallel_4"]
DEFAULT_REPS = 5  # 1 warmup + 4 measured

# Baseline normalized knobs, matched-pair across tools (R130).
# `rag_prefill` overrides `ctx` to 8192 so the 8k corpus fits.
BASE_NORMALIZED_KNOBS = NormalizedKnobs(
  ctx=4096,
  n_gpu_layers=999,
  flash_attn=True,
  kv_cache_type="f16",
  batch_size=512,
  ubatch_size=512,
)
RAG_PREFILL_CTX = 8192


def _git_sha() -> Optional[str]:
  try:
    out = subprocess.run(
      ["git", "rev-parse", "HEAD"],
      capture_output=True,
      text=True,
      timeout=3,
      check=False,
    )
    return out.stdout.strip() or None
  except (subprocess.TimeoutExpired, OSError):
    return None


def _csv_list(value: str) -> list[str]:
  return [v.strip() for v in value.split(",") if v.strip()]


def build_arg_parser() -> argparse.ArgumentParser:
  parser = argparse.ArgumentParser(
    prog="bench-end-to-end",
    description="Suite B end-to-end benchmark across llamastash, llamacpp, ollama, lmstudio.",
  )
  parser.add_argument(
    "--dry-run",
    action="store_true",
    help="Print the planned matrix + exit; spawn nothing.",
  )
  parser.add_argument(
    "--out-dir",
    type=Path,
    default=Path("docs/benchmarks/runs"),
    help="Per-host runs subdirectory root.",
  )
  parser.add_argument(
    "--tools",
    type=_csv_list,
    default=DEFAULT_TOOLS,
    help="Comma-separated tool list (subset of llamastash,llamacpp,ollama,lmstudio).",
  )
  parser.add_argument(
    "--models",
    type=_csv_list,
    default=DEFAULT_MODELS,
    help="Comma-separated model size classes (small, mid, large_dense, large_moe).",
  )
  parser.add_argument(
    "--modes",
    type=_csv_list,
    default=DEFAULT_MODES,
    help="Comma-separated launch modes (defaults, normalized).",
  )
  parser.add_argument(
    "--workloads",
    type=_csv_list,
    default=DEFAULT_WORKLOADS,
    help="Comma-separated workloads (chat_turn, rag_prefill, agent_decode, parallel_4).",
  )
  parser.add_argument(
    "--reps",
    type=int,
    default=DEFAULT_REPS,
    help="Total reps per cell (1 warmup + N-1 measured).",
  )
  parser.add_argument(
    "--render",
    action="store_true",
    help="After writing the JSON, invoke render.py for today's date.",
  )
  parser.add_argument(
    "--continue-on-cell-error",
    action="store_true",
    default=True,
    help="Record cell failures and proceed (default). Pair with --no-continue-on-cell-error to abort.",
  )
  parser.add_argument(
    "--no-continue-on-cell-error",
    dest="continue_on_cell_error",
    action="store_false",
  )
  return parser


def _planned_matrix(args: argparse.Namespace) -> list[dict]:
  """Cartesian product of (tool, model, mode, workload). Used by
  --dry-run and by the real execution loop."""
  matrix = []
  for tool in args.tools:
    for model in args.models:
      for mode in args.modes:
        for workload in args.workloads:
          matrix.append(
            {
              "tool": tool,
              "model": model,
              "mode": mode,
              "workload": workload,
              "reps": args.reps,
            }
          )
  return matrix


def _output_path(out_dir: Path, host_id: str, started_at: dt.datetime, git_sha: Optional[str]) -> Path:
  date = started_at.strftime("%Y-%m-%d")
  sha = (git_sha or "nosha")[:12]
  return out_dir / host_id / f"{date}-{sha}.json"


def _knobs_for_workload(workload: str) -> NormalizedKnobs:
  base = BASE_NORMALIZED_KNOBS
  if workload == "rag_prefill":
    return NormalizedKnobs(
      ctx=RAG_PREFILL_CTX,
      n_gpu_layers=base.n_gpu_layers,
      flash_attn=base.flash_attn,
      kv_cache_type=base.kv_cache_type,
      batch_size=base.batch_size,
      ubatch_size=base.ubatch_size,
    )
  return base


def _unfair_knobs_for_driver(driver: Driver, applied: NormalizedKnobs) -> list[str]:
  supported = driver.normalized_knobs_supported()
  requested = {k for k, v in applied.__dict__.items() if v is not None and v is not False}
  return sorted(requested - supported)


def _request_model_name(tool: str, handle_name: str) -> str:
  """The string passed in the OpenAI-compatible `model` field. Most
  tools accept any string; Ollama and LM Studio require the
  loaded-model identifier (which is what `handle.name` already is)."""
  if tool in {"llamacpp", "llamastash"}:
    return Path(handle_name).name
  return handle_name


def _determinism_from_single(
  prompt_text: Optional[str],
  output_text: Optional[str],
  n_tokens: int,
) -> Determinism:
  """A single-sample Determinism record. Cross-tool comparison is
  the renderer's job (it joins cells by model+workload+backend); we
  capture the prompt/output hashes for that join."""
  if not (prompt_text and output_text):
    return Determinism()
  return fairness_check(
    [
      FairnessSample(
        tool="this-cell",
        prompt_text=prompt_text,
        output_text=output_text,
        n_compared_tokens=n_tokens,
      )
    ]
  )


def _spec_path_for(spec: ModelSpec) -> Path:
  """ModelSpec → local file path the driver should open. Re-reads
  the env var so we don't have to thread the path through the
  matrix loop."""
  env_var = SLOT_ENV_VARS[spec.size_class]
  raw = os.environ.get(env_var, "")
  return Path(raw).expanduser()


async def _run_cell(
  tool: str,
  model_spec: ModelSpec,
  mode_str: str,
  workload: str,
  reps: int,
) -> Cell:
  """Run one (tool, model, mode, workload) cell. Always returns a
  Cell; failures land in `notes` with measured_rep_count=0."""
  mode = Mode.DEFAULTS if mode_str == "defaults" else Mode.NORMALIZED
  knobs = _knobs_for_workload(workload) if mode == Mode.NORMALIZED else None
  model_path = _spec_path_for(model_spec)

  driver = make_driver(tool)
  notes_parts: list[str] = []
  rep_records: list[Rep] = []
  unfair: list[str] = []
  argv_recorded: list[str] = []
  prompt_for_det: Optional[str] = None
  output_for_det: Optional[str] = None
  n_tokens_for_det = 0

  try:
    handle = driver.prepare_model(model_path, mode)
    base_url = driver.start(handle, mode, knobs)
    argv_recorded = list(driver.recorded_argv())
    if knobs is not None:
      unfair = _unfair_knobs_for_driver(driver, knobs)

    model_name_for_request = _request_model_name(tool, handle.name)
    try:
      async with httpx.AsyncClient(timeout=600.0) as client:
        for rep_index in range(reps):
          is_warmup = rep_index == 0
          try:
            result: WorkloadResult = await run_workload(
              workload,
              base_url,
              model_name_for_request,
              rep_index=rep_index,
              is_warmup=is_warmup,
              client=client,
            )
          except Exception as exc:  # noqa: BLE001 - record + continue
            result = WorkloadResult(
              rep_index=rep_index,
              is_warmup=is_warmup,
              prompt_text="",
              error=f"workload-raised: {type(exc).__name__}: {exc}",
            )
          rep_records.append(result.to_rep())
          if not is_warmup and result.error is None and result.output_text:
            prompt_for_det = result.prompt_text
            output_for_det = result.output_text
            n_tokens_for_det = result.decode_tokens or n_tokens_for_det
    finally:
      try:
        driver.stop()
      except Exception as exc:  # noqa: BLE001 - cleanup must not raise
        notes_parts.append(f"stop-error: {type(exc).__name__}: {exc}")
  except DriverError as exc:
    notes_parts.append(f"driver-error: {type(exc).__name__}: {exc}")
  except Exception as exc:  # noqa: BLE001 - infra-layer fault
    notes_parts.append(f"cell-error: {type(exc).__name__}: {exc}")
    try:
      driver.stop()
    except Exception:
      pass

  return Cell(
    tool=tool,  # type: ignore[arg-type]
    model=model_spec,
    mode=mode_str,  # type: ignore[arg-type]
    workload=workload,  # type: ignore[arg-type]
    argv_recorded=argv_recorded,
    reps=rep_records,
    summary=summarize(rep_records),
    unfair_knobs=unfair,
    determinism=_determinism_from_single(prompt_for_det, output_for_det, n_tokens_for_det),
    notes="; ".join(notes_parts),
  )


def _validate_and_resolve_models(args: argparse.Namespace) -> dict[str, ModelSpec]:
  try:
    return resolve_slots(args.models, require_all=True)
  except (ModelSlotMissing, ModelFileMissing) as exc:
    print(f"==> ERROR: {exc}", file=sys.stderr)
    print(
      "    set the size-class env var(s) to absolute paths of local GGUF files, e.g.:",
      file=sys.stderr,
    )
    print(
      "    LLAMASTASH_BENCH_MODELS_SMALL=/path/to/small.gguf "
      "LLAMASTASH_BENCH_MODELS_MID=/path/to/mid.gguf ...",
      file=sys.stderr,
    )
    raise SystemExit(2) from exc


async def _execute_matrix(
  args: argparse.Namespace,
  models: dict[str, ModelSpec],
) -> list[Cell]:
  matrix = _planned_matrix(args)
  cells: list[Cell] = []
  total = len(matrix)
  for i, plan in enumerate(matrix, start=1):
    tool, cls, mode, wl, reps = plan["tool"], plan["model"], plan["mode"], plan["workload"], plan["reps"]
    spec = models[cls]
    print(
      f"==> [{i}/{total}] tool={tool} model={cls} mode={mode} workload={wl} reps={reps}",
      file=sys.stderr,
    )
    try:
      cell = await _run_cell(tool, spec, mode, wl, reps)
    except Exception as exc:  # noqa: BLE001
      print(f"    !! cell raised: {type(exc).__name__}: {exc}", file=sys.stderr)
      if not args.continue_on_cell_error:
        raise
      continue
    cells.append(cell)
    if cell.notes:
      print(f"    notes: {cell.notes}", file=sys.stderr)
    if cell.summary.measured_rep_count and cell.summary.decode_tps_mean and cell.summary.ttft_ms_mean:
      print(
        f"    decode={cell.summary.decode_tps_mean:.1f} tok/s "
        f"ttft={cell.summary.ttft_ms_mean:.0f} ms "
        f"({cell.summary.measured_rep_count} measured)",
        file=sys.stderr,
      )
  return cells


def _invoke_renderer(date_str: str, runs_dir: Path) -> None:
  print(f"==> invoking renderer for {date_str}", file=sys.stderr)
  rc = subprocess.run(
    [
      sys.executable,
      "-m",
      "scripts.bench.end_to_end.render",
      "--date",
      date_str,
      "--runs-dir",
      str(runs_dir),
    ],
    check=False,
  ).returncode
  if rc != 0:
    print(f"    renderer exited {rc}", file=sys.stderr)


def main(argv: Optional[list[str]] = None) -> int:
  args = build_arg_parser().parse_args(argv)

  host = capture_host()
  provenance = capture_provenance()
  started_at = dt.datetime.now(dt.timezone.utc)
  matrix = _planned_matrix(args)

  print(f"==> bench-end-to-end on host={host.host_id} backend={host.gpu_backend}", file=sys.stderr)
  print(f"    tools     : {','.join(args.tools)}", file=sys.stderr)
  print(f"    models    : {','.join(args.models)}", file=sys.stderr)
  print(f"    modes     : {','.join(args.modes)}", file=sys.stderr)
  print(f"    workloads : {','.join(args.workloads)}", file=sys.stderr)
  print(f"    reps/cell : {args.reps} (1 warmup + {args.reps - 1} measured)", file=sys.stderr)
  print(f"    planned   : {len(matrix)} cells", file=sys.stderr)

  if args.dry_run:
    for cell in matrix:
      print(
        f"    plan tool={cell['tool']} model={cell['model']} mode={cell['mode']} "
        f"workload={cell['workload']} reps={cell['reps']}",
        file=sys.stderr,
      )
    return 0

  models = _validate_and_resolve_models(args)
  print(
    "    resolved  : "
    + ", ".join(f"{cls}={models[cls].hf_file}" for cls in args.models),
    file=sys.stderr,
  )

  cells = asyncio.run(_execute_matrix(args, models))

  finished_at = dt.datetime.now(dt.timezone.utc)
  report = RunReport(
    suite="end_to_end",
    host=host,
    provenance=provenance,
    started_at_utc=started_at.isoformat(),
    finished_at_utc=finished_at.isoformat(),
    git_sha=_git_sha(),
    cells=cells,
  )

  out_path = _output_path(args.out_dir, host.host_id, started_at, report.git_sha)
  out_path.parent.mkdir(parents=True, exist_ok=True)
  out_path.write_text(report.model_dump_json(indent=2) + "\n")
  print(
    f"==> wrote {out_path} ({len(cells)} cells, "
    f"{(finished_at - started_at).total_seconds():.0f}s wall-clock)",
    file=sys.stderr,
  )

  if args.render:
    _invoke_renderer(started_at.strftime("%Y-%m-%d"), args.out_dir)
  return 0


if __name__ == "__main__":
  raise SystemExit(main())
