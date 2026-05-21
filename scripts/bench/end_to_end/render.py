"""Aggregation + rendering pipeline (Unit 5).

Reads all ``docs/benchmarks/runs/**/*.json`` (Suite B) plus the
per-host overhead JSONs, validates them against schema v1, applies
the variance gate, writes a dated Markdown results page, and
prepends a link to ``docs/benchmarks/index.md``.

The variance gate (R140):
- stddev / mean × 100 ≤ 10%  → published, no annotation
- 10% < x ≤ 25%             → flagged: ± inline, excluded from headline
- > 25%                     → dropped: footer note, "re-run needed"

Schema rejection is fatal: any source JSON the validator rejects
exits non-zero rather than silently skipping. The renderer is the
contract that keeps producer/consumer in sync (the variance gate
itself can't detect schema drift).
"""
from __future__ import annotations

import argparse
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Optional

from pydantic import ValidationError

from . import charts
from .schema import Cell, RunReport

FLAG_PCT = 10.0
DROP_PCT = 25.0

DEFAULT_RUNS_DIR = Path("docs/benchmarks/runs")
DEFAULT_RESULTS_DIR = Path("docs/benchmarks")
DEFAULT_INDEX_FILE = Path("docs/benchmarks/index.md")


# ---- Status classification ---------------------------------------


class CellStatus:
  CLEAN = "clean"
  FLAGGED = "flagged"
  DROPPED = "dropped"


def classify_cell(cell: Cell) -> str:
  """Pick the worst-status across the cell's primary metrics. Any
  metric over the drop threshold drops the cell. Otherwise any
  metric over the flag threshold flags it. Otherwise CLEAN."""
  candidates = [
    cell.summary.ttft_ms_stddev_pct,
    cell.summary.prompt_tps_stddev_pct,
    cell.summary.decode_tps_stddev_pct,
  ]
  worst = max((c for c in candidates if c is not None), default=0.0)
  if worst > DROP_PCT:
    return CellStatus.DROPPED
  if worst > FLAG_PCT:
    return CellStatus.FLAGGED
  return CellStatus.CLEAN


# ---- Discovery + validation --------------------------------------


@dataclass
class LoadedRun:
  source_path: Path
  report: RunReport


def discover_runs(runs_dir: Path) -> list[Path]:
  if not runs_dir.exists():
    return []
  return sorted(p for p in runs_dir.glob("**/*.json") if p.is_file())


def load_runs(paths: Iterable[Path]) -> list[LoadedRun]:
  """Parse + validate every path. Raises on schema mismatch — the
  renderer never silently drops a source the variance gate would
  otherwise weigh in on."""
  loaded: list[LoadedRun] = []
  for p in paths:
    try:
      raw = p.read_text()
      report = RunReport.model_validate_json(raw)
    except ValidationError as exc:
      raise ValidationError.from_exception_data(
        title=f"schema rejection: {p}",
        line_errors=exc.errors(),  # type: ignore[arg-type]
      )
    loaded.append(LoadedRun(source_path=p, report=report))
  return loaded


# ---- Cell grouping for tables -----------------------------------


def group_cells_by_model_workload(runs: list[LoadedRun]) -> dict[tuple[str, str], list[Cell]]:
  """Cells across all runs, keyed by (model.size_class, workload)
  for cross-tool comparison."""
  by_key: dict[tuple[str, str], list[Cell]] = {}
  for run in runs:
    for cell in run.report.cells:
      key = (cell.model.size_class, cell.workload)
      by_key.setdefault(key, []).append(cell)
  return by_key


# ---- Markdown rendering -----------------------------------------


def _format_metric(mean: Optional[float], stddev_pct: Optional[float], unit: str) -> str:
  if mean is None:
    return "—"
  if stddev_pct is not None and stddev_pct > FLAG_PCT and stddev_pct <= DROP_PCT:
    return f"{mean:,.1f} {unit} ±{stddev_pct:.0f}%"
  if stddev_pct is not None:
    return f"{mean:,.1f} {unit}"
  return f"{mean:,.1f} {unit}"


def _tool_pretty(tool: str) -> str:
  return {
    "llamastash": "LlamaStash",
    "llamacpp": "llama-server (raw)",
    "ollama": "Ollama",
    "lmstudio": "LM Studio",
  }.get(tool, tool)


def render_cell_table(cells: list[Cell]) -> str:
  """Markdown table for one (model, workload) group. Includes the
  ± inline for flagged cells. Dropped cells are NOT in this table —
  they go to the footer section."""
  header = "| Tool | Mode | decode tok/s | TTFT | prompt tok/s | reps | status |\n"
  sep = "|---|---|---|---|---|---|---|\n"
  rows = []
  for c in sorted(cells, key=lambda x: (x.tool, x.mode)):
    status = classify_cell(c)
    if status == CellStatus.DROPPED:
      continue
    decode = _format_metric(c.summary.decode_tps_mean, c.summary.decode_tps_stddev_pct, "tok/s")
    ttft = _format_metric(c.summary.ttft_ms_mean, c.summary.ttft_ms_stddev_pct, "ms")
    prompt = _format_metric(c.summary.prompt_tps_mean, c.summary.prompt_tps_stddev_pct, "tok/s")
    status_str = "flagged" if status == CellStatus.FLAGGED else "ok"
    unfair = f" ({', '.join(c.unfair_knobs)})" if c.unfair_knobs else ""
    rows.append(
      f"| {_tool_pretty(c.tool)}{unfair} | {c.mode} | {decode} | {ttft} | {prompt} "
      f"| {c.summary.measured_rep_count} | {status_str} |"
    )
  return header + sep + "\n".join(rows) + "\n"


def render_dropped_footer(cells_all: list[Cell]) -> str:
  """One-line entries for every dropped cell across the whole run.
  Surfaces them visibly rather than burying."""
  dropped = [c for c in cells_all if classify_cell(c) == CellStatus.DROPPED]
  if not dropped:
    return ""
  lines = [
    "## Re-run needed",
    "",
    "These cells exceeded the 25% stddev drop threshold and were excluded:",
    "",
  ]
  for c in sorted(dropped, key=lambda x: (x.model.size_class, x.workload, x.tool)):
    stddevs = [
      f"decode {c.summary.decode_tps_stddev_pct:.0f}%" if c.summary.decode_tps_stddev_pct else "",
      f"ttft {c.summary.ttft_ms_stddev_pct:.0f}%" if c.summary.ttft_ms_stddev_pct else "",
    ]
    suffix = ", ".join(s for s in stddevs if s)
    lines.append(
      f"- `{c.model.size_class}` / `{c.workload}` / {_tool_pretty(c.tool)} "
      f"({c.mode}) — {suffix}"
    )
  return "\n".join(lines) + "\n"


def render_determinism_callouts(cells_all: list[Cell]) -> str:
  mismatched = [c for c in cells_all if c.determinism.determinism_mismatch]
  if not mismatched:
    return ""
  lines = [
    "## Determinism mismatches",
    "",
    "Per-cell fairness check (same-backend only — cross-backend "
    "differences are logged but not failed):",
    "",
  ]
  for c in mismatched:
    lines.append(
      f"- `{c.model.size_class}` / `{c.workload}` / {_tool_pretty(c.tool)} "
      f"({c.mode}) — {c.determinism.notes or 'token-hash divergence'}"
    )
  return "\n".join(lines) + "\n"


# ---- Top-level render ------------------------------------------


def render_results_page(
  runs: list[LoadedRun],
  date: str,
  charts_dir: Path,
  primary_backend: Optional[str] = None,
) -> str:
  """Build the full Markdown body. Charts are rendered into
  `charts_dir` and referenced via relative `<img>` markdown."""
  if not runs:
    return f"# Bench results — {date}\n\n_no source data found in runs/_\n"

  all_cells: list[Cell] = [c for run in runs for c in run.report.cells]
  hosts = sorted({r.report.host.host_id for r in runs})
  backends = sorted({r.report.host.gpu_backend for r in runs})
  primary_backend = primary_backend or backends[0]

  out_lines: list[str] = [
    f"# Bench results — {date}",
    "",
    f"_Source: {len(runs)} run file(s) from host(s) {', '.join(hosts)} "
    f"on backend(s) {', '.join(backends)}._",
    "",
    "See [methodology.md](methodology.md) for the matched-pair settings "
    "policy, the variance-gate rules, and the conflict-of-interest "
    "disclaimer. Charts are deterministic SVG — re-render from the "
    "source JSONs to verify.",
    "",
  ]

  grouped = group_cells_by_model_workload(runs)
  for (model, workload), cells in sorted(grouped.items()):
    out_lines.append(f"## {model} — {workload}")
    out_lines.append("")
    headline_cells = [c for c in cells if classify_cell(c) != CellStatus.DROPPED]
    chart_dir = charts_dir
    decode_chart = chart_dir / f"{model}-{workload}-decode.svg"
    ttft_chart = chart_dir / f"{model}-{workload}-ttft.svg"
    charts.render_decode_tps_bar(
      headline_cells, model_label=model, backend_label=primary_backend, workload=workload, out_path=decode_chart
    )
    charts.render_ttft_bar(
      headline_cells, model_label=model, backend_label=primary_backend, workload=workload, out_path=ttft_chart
    )
    rel = chart_dir.name
    out_lines.append(f"![decode tok/s]({rel}/{decode_chart.name})")
    out_lines.append("")
    out_lines.append(f"![TTFT]({rel}/{ttft_chart.name})")
    out_lines.append("")
    out_lines.append(render_cell_table(cells))
    out_lines.append("")

  out_lines.append(render_determinism_callouts(all_cells))
  out_lines.append(render_dropped_footer(all_cells))

  return "\n".join(line for line in out_lines if line is not None)


def update_index(index_path: Path, date: str, results_path: Path) -> None:
  """Prepend a one-line entry to docs/benchmarks/index.md under the
  ``## Results`` section. Idempotent: re-running the same (date,
  path) doesn't add duplicate lines."""
  if not index_path.exists():
    index_path.write_text(
      "# LlamaStash benchmarks\n\n## Results\n\n- "
      f"[{date}]({results_path.name})\n"
    )
    return
  body = index_path.read_text()
  rel = results_path.name
  entry = f"- [{date}]({rel})"
  if entry in body:
    return
  marker = "## Results"
  if marker not in body:
    index_path.write_text(body.rstrip() + "\n\n" + marker + "\n\n" + entry + "\n")
    return
  before, _, after = body.partition(marker)
  # Drop a placeholder line if present (the initial Unit-7 "no page yet" note).
  after_lines = after.splitlines()
  cleaned: list[str] = []
  skipped_placeholder = False
  for line in after_lines:
    if not skipped_placeholder and "no results page yet" in line.lower():
      skipped_placeholder = True
      continue
    cleaned.append(line)
  new = before + marker + "\n\n" + entry + "\n" + "\n".join(cleaned)
  if not new.endswith("\n"):
    new += "\n"
  index_path.write_text(new)


# ---- CLI --------------------------------------------------------


def build_arg_parser() -> argparse.ArgumentParser:
  p = argparse.ArgumentParser(
    prog="bench-render",
    description="Render dated results page from docs/benchmarks/runs/**/*.json",
  )
  p.add_argument("--date", required=True, help="YYYY-MM-DD for the dated page.")
  p.add_argument("--runs-dir", type=Path, default=DEFAULT_RUNS_DIR)
  p.add_argument("--out-dir", type=Path, default=DEFAULT_RESULTS_DIR)
  p.add_argument("--index", type=Path, default=DEFAULT_INDEX_FILE)
  p.add_argument(
    "--primary-backend",
    default=None,
    help="Override the autopicked primary backend label (used in chart titles).",
  )
  p.add_argument("--dry-run", action="store_true", help="Print planned actions; write nothing.")
  return p


def main(argv: Optional[list[str]] = None) -> int:
  args = build_arg_parser().parse_args(argv)
  paths = discover_runs(args.runs_dir)
  print(f"==> discovered {len(paths)} run JSON(s) under {args.runs_dir}", file=sys.stderr)
  if not paths:
    print("    (nothing to render — exiting cleanly)", file=sys.stderr)
    return 0
  runs = load_runs(paths)

  results_file = args.out_dir / f"results-{args.date}.md"
  charts_dir = args.out_dir / f"results-{args.date}"
  if args.dry_run:
    print(f"    would write {results_file}", file=sys.stderr)
    print(f"    would write charts under {charts_dir}/", file=sys.stderr)
    print(f"    would update {args.index}", file=sys.stderr)
    return 0

  args.out_dir.mkdir(parents=True, exist_ok=True)
  charts_dir.mkdir(parents=True, exist_ok=True)
  body = render_results_page(runs, args.date, charts_dir, args.primary_backend)
  results_file.write_text(body)
  update_index(args.index, args.date, results_file)
  print(f"==> wrote {results_file}", file=sys.stderr)
  return 0


if __name__ == "__main__":
  raise SystemExit(main())


__all__ = [
  "CellStatus",
  "FLAG_PCT",
  "DROP_PCT",
  "LoadedRun",
  "classify_cell",
  "discover_runs",
  "group_cells_by_model_workload",
  "load_runs",
  "render_cell_table",
  "render_determinism_callouts",
  "render_dropped_footer",
  "render_results_page",
  "update_index",
]
