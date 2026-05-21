"""Suite B (cross-tool end-to-end comparison) orchestrator.

Walks the full matrix:

    backends (auto-detected, single per host)
      × models   {small, mid, large_dense, [large_moe]}
      × tools    {llamastash, llamacpp, ollama, lmstudio}
      × modes    {defaults, normalized}
      × workloads (4)
      × reps     (1 warmup + N measured)

Driver and workload modules are imported lazily so the dry-run path
works without httpx / pydantic-extras / matplotlib being importable.

This file is intentionally small. The heavy lifting lives in:

    drivers/    — per-tool spawn + readiness + cleanup        (Unit 2)
    workloads.py — chat_turn / rag_prefill / agent_decode /
                   parallel_4 against an OpenAI-compatible URL (Unit 3)
    metrics.py   — TTFT / tps / variance / determinism aggregators (Unit 3)
    render.py    — variance gate + Markdown + SVG charts     (Unit 5)
"""
from __future__ import annotations

import argparse
import datetime as dt
import subprocess
import sys
from pathlib import Path
from typing import Optional

from .provenance import capture_host, capture_provenance
from .schema import RunReport

DEFAULT_TOOLS = ["llamastash", "llamacpp", "ollama", "lmstudio"]
DEFAULT_MODELS = ["small", "mid", "large_dense"]
DEFAULT_MODES = ["defaults", "normalized"]
DEFAULT_WORKLOADS = ["chat_turn", "rag_prefill", "agent_decode", "parallel_4"]
DEFAULT_REPS = 5  # 1 warmup + 4 measured


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
  return parser


def _planned_matrix(args: argparse.Namespace) -> list[dict]:
  """Cartesian product of (tool, model, mode, workload). Used by
  --dry-run and by the real loop in Unit 8."""
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

  # Cell execution lands in Unit 8. Until then, refuse to silently
  # write an empty RunReport — the maintainer should see "wire not
  # complete" rather than think the harness succeeded.
  print(
    "==> driver / workload wiring is not complete (Unit 8 lands cell execution).",
    file=sys.stderr,
  )
  print(
    "    Use --dry-run to validate the matrix until then.",
    file=sys.stderr,
  )

  finished_at = dt.datetime.now(dt.timezone.utc)
  report = RunReport(
    suite="end_to_end",
    host=host,
    provenance=provenance,
    started_at_utc=started_at.isoformat(),
    finished_at_utc=finished_at.isoformat(),
    git_sha=_git_sha(),
    cells=[],
    notes="cells not yet wired — see Unit 8 in docs/plans/2026-05-21-001-feat-benchmark-harness-plan.md",
  )

  out_path = _output_path(args.out_dir, host.host_id, started_at, report.git_sha)
  out_path.parent.mkdir(parents=True, exist_ok=True)
  out_path.write_text(report.model_dump_json(indent=2) + "\n")
  print(f"==> wrote provenance-only RunReport to {out_path}", file=sys.stderr)
  return 0


if __name__ == "__main__":
  raise SystemExit(main())
