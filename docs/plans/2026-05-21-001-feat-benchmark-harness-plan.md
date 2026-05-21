---
title: "feat: Cross-Tool Benchmark Harness (Suite A overhead + Suite B end-to-end)"
type: feat
status: active
date: 2026-05-21
origin: docs/brainstorms/2026-05-21-benchmark-harness-requirements.md
---

# feat: Cross-Tool Benchmark Harness (Suite A overhead + Suite B end-to-end)

## Overview

A two-suite, maintainer-run Python benchmark harness with accompanying docs and a conditional launch post. **Suite A** is the overhead regression check — `llamastash start` vs raw `llama-server` on the same model + same effective argv, scored against two-tier thresholds. **Suite B** is the cross-tool end-to-end comparison — LlamaStash, raw `llama-server`, Ollama, and LM Studio driven through their OpenAI-compatible HTTP endpoints across a full backend × model-size × settings × workload matrix. Both suites share infrastructure (drivers, workloads, metrics, output schema, rendering). Output lives under `docs/benchmarks/`; CI is explicitly out of scope per the brainstorm's R125 decision.

## Problem Frame

The README's "Why" leads with **"Heavy abstractions (Ollama, LM Studio) hide llama.cpp; raw `llama-server` use is tedious. LlamaStash is a fast, transparent launcher…"** That positioning currently has zero quantitative backing. Two architecturally-distinct questions hide inside it: (1) does LlamaStash in fact add ~0 inference overhead vs raw `llama-server`? (2) how does LlamaStash-as-shipped compare to Ollama / LM Studio on the same model + hardware? The first protects the architectural claim going forward; the second is the question prospective users actually ask. This plan turns the brainstorm decisions (R121–R150) into an implementation-ready sequence. (see origin: `docs/brainstorms/2026-05-21-benchmark-harness-requirements.md`)

## Requirements Trace

The plan satisfies the brainstorm's R121–R150 in full. Compact mapping (units listed below; each unit's section repeats its R-coverage):

- R121–R125 — Suite A (overhead) → Unit 4 (env-var hook) + Unit 6 (overhead orchestrator + thresholds)
- R126–R136 — Suite B core → Units 1 (skeleton), 2 (drivers), 3 (workloads/metrics), 5 (render)
- R137 — Tool install out of scope → Unit 2 driver "tool-not-found" guards + Unit 7 prerequisites doc
- R138–R140 — Rendering, versioning, variance gate → Unit 5
- R141 — Fairness self-check → Unit 3 (metrics layer enforces determinism check)
- R142–R143 — Methodology + benchmarks index + README section → Unit 7
- R144 — Launch post (conditional) → Unit 9
- R145–R150 — Non-goals → "Scope Boundaries" below
- Q1, Q2, Q6 — execution-time unknowns → resolved in Unit 8 (first end-to-end run)
- Q3 — `large-dense` vs `large-moe` per host → resolved in Unit 8 dry run
- Q4 — per-tool `llama.cpp` commit recording → Unit 1 (provenance), best-effort
- Q5 — CI runner cadence → already resolved (no CI)

## Scope Boundaries

Carrying forward the brainstorm's Non-Goals (R145–R150) and adding plan-level exclusions:

- **Model-quality comparison** (R145). Speed and resource cost only — no HumanEval / MMLU / Aider runs.
- **GUI / UX comparison** (R146). LM Studio's GUI vs LlamaStash's TUI is a separate brainstorm.
- **Native non-llama.cpp engines** (R147). LM Studio's MLX, vLLM, mlc-llm, exllamav2 are out. Normalized mode forces LM Studio's llama.cpp path; MLX is left for a future "Apple Silicon engine comparison."
- **Cloud / hosted endpoints** (R148). Local-only.
- **Windows** (R149). Matches LlamaStash's own platform coverage.
- **"Try to make LlamaStash win"** (R150). Unflattering numbers ship truthfully.
- **CI integration** (R125). Both suites run on the maintainer's hardware, never in CI.
- **Adding tools beyond the four named** (R126). Jan, GPT4All, llamafile, KoboldCpp are explicit follow-ups, not v1.
- **Pre-installing tools**. The harness verifies presence and exits with a hint; install instructions live in the methodology doc (R137).
- **Tool-specific protocol shortcuts** (R127). All four tools are driven exclusively through `/v1/chat/completions` (and `/v1/embeddings` if an embedding workload is added later).

## Context & Research

### Relevant Code and Patterns

- **`scripts/measure-overhead-band.py`** — strongest local precedent. Python + per-backend GPU samplers (`_sample_nvidia_total`, `_sample_amd_total`, `_sample_metal_per_process`), shell wrapper around it, output JSON keyed by `<host>-<backend>-<ts>.json`. Unit 3 reuses the samplers (refactor into a shared `gpu_sampler.py` module).
- **`scripts/regenerate-benchmark-snapshot.py`** + **`scripts/measure-overhead-band.sh`** — bash-wraps-python pattern. Unit 1 / Unit 6 follow the same shape.
- **`Makefile` `.venv/bin/python` target** (lines 28–36) — `uv` preferred, stdlib `venv + pip` fallback, dependencies pinned in `scripts/requirements.txt`. Unit 1 extends `requirements.txt` with `httpx`, `pydantic`, `matplotlib`.
- **`src/launch/params.rs`** — the layered resolver chain `user > last_used > arch_defaults > builtin > model_default` (see comment at line 142). Unit 4 hooks here to honor `LLAMASTASH_BENCH_DISABLE_DEFAULTS`.
- **`src/launch/defaults_table.rs`** — the static arch-defaults table. Unit 4's env-var branch skips this layer when set.
- **`tests/fixtures/fake_llama_server.rs`** — gated by `--features test-fixtures`. Unit 3 / Unit 6 smoke tests can target it for harness wiring tests (without measuring real perf).
- **`tests/measure-overhead-band.py` invocation idiom** — backend auto-detect, per-host short hostname tagging, `data/overhead-band-measurements/` output layout. Unit 1 mirrors with `docs/benchmarks/runs/<host-id>/`.
- **`AGENTS.md` §Scope boundaries** — confirms the loopback-only / same-UID contract this plan does NOT touch. The bench harness is external and never alters the daemon's IPC or socket surface.
- **`AGENTS.md` §Docs stay in sync with code** — the `README.md` + `docs/usage.md` updates land in the same change as the env-var hook (Unit 4), not a follow-up.

### Institutional Learnings

`docs/solutions/` does not exist yet. No prior learnings memos to cite. Note for future: the brainstorm itself + this plan are the institutional record.

### External References

External research deliberately skipped per Phase 1.2 of the planning workflow. Justification: the user has hands-on context, the local pattern (`measure-overhead-band.py`) is strong, all four tools' OpenAI-compatible APIs are well-documented in their own repos, and the brainstorm already resolved the philosophical questions (matched-pair settings, four workloads, variance gate). Driver implementations will lean on per-tool docs at code-time, not plan-time.

## Key Technical Decisions

- **One Python entry point per suite, sharing modules.** `scripts/bench/end-to-end/orchestrator.py` and `scripts/bench/overhead/orchestrator.py` are thin orchestrators that compose drivers, workloads, metrics, and rendering from the shared `scripts/bench/end-to-end/` package. Avoids two parallel codebases; lets Suite A inherit Suite B's metric machinery for free.
- **Driver protocol over inheritance.** Per-tool drivers (R136) implement a `Driver` Protocol — no abstract base class. New tools = one new file under `drivers/`, no orchestrator changes. Mirrors how `src/launch/binary.rs` keeps backend variants behind a single resolution function.
- **`LLAMASTASH_BENCH_DISABLE_DEFAULTS` as an env var, not a CLI flag.** Mirrors the existing `LLAMASTASH_SOCKET`, `LLAMASTASH_STATE_DIR`, `HF_HOME` family. The bench script sets it; humans never touch it; no CLI surface change, no help-text noise, no risk of users discovering it as "the way to launch a model fast."
- **Schema v1 with Pydantic.** Catches schema drift cheaply; v2 happens when a new metric or cell field is added — never silently. Drift across renderer ↔ producer is the failure mode the variance gate (R140) can't catch on its own.
- **Loose Python scripts, not a `pyproject.toml` package.** Matches the existing `scripts/*.py` convention. Tests under `scripts/bench/end-to-end/tests/` run via `pytest scripts/bench/` from the venv. No need to ship the harness as a wheel.
- **Output lives in `docs/benchmarks/`, not `data/`.** Diverges from `data/overhead-band-measurements/` precedent intentionally — these JSONs back the public docs page (R134, R142), so they belong next to the docs that consume them. Per-host subdirectory keeps merging community contributions trivial (R135).
- **Rendering is offline and deterministic.** Matplotlib SVG export, no JS, no interactive widgets. Reproducible from raw JSON; reviewers can re-render to verify the chart wasn't doctored.
- **Suite A reuses Suite B's modules.** Suite A is "Suite B's chat-turn workload, run twice on the same hardware (raw vs llamastash), with a threshold check on the deltas." Building Suite B first means Suite A is mostly orchestration.
- **Determinism check at the within-backend layer only** (R141 as edited). Token-ID equality across backends is never asserted; floating-point variance across CUDA / Metal / ROCm / CPU is real and not a bug.

## Open Questions

### Resolved During Planning

- **Should the bench harness be a separate Python package (`pyproject.toml`) or loose scripts?** Resolved: loose scripts, matching `scripts/measure-overhead-band.py`. Single convention across the repo. (see origin: §Common infrastructure R136)
- **Should the env-var hook be a CLI flag (`--bench-mode`) instead?** Resolved: env var. Avoids surfacing it in `--help` and matches the existing `LLAMASTASH_*` family. The bench script sets it; humans don't.
- **Where do the per-backend GPU samplers live?** Resolved: refactor `scripts/measure-overhead-band.py::_sample_*` into a shared `scripts/bench/end-to-end/gpu_sampler.py` and have `measure-overhead-band.py` import from there. Avoids divergence between the two harnesses. (Slight risk: touching the existing script ahead of Unit 8's snapshot rerun — Unit 3 covers the refactor with a regression test against the existing JSON output shape.)
- **Should Suite A's regression check be one workload or four?** Resolved: one workload (`chat-turn`) for v1 per R124. The brainstorm raised this as P4 but the maintainer kept R124 as-is. Revisit only if a real regression slips through the gate.
- **TTFT for "cold launch" — does it include lazy-load on Ollama / LM Studio?** Resolved per Q6 brainstorm proposal: report **both** as separate metrics (`ttft_ms_first_request` and `ttft_ms_post_load`). The renderer chooses which to chart per workload.

### Deferred to Implementation

- **Q1 — LM Studio normalization ceiling.** Which knobs the `lms` CLI refuses to expose. Discovered in Unit 8 dry run; unfair knobs get logged per R130 mechanism. Methodology doc updated post-dry-run with the actual list.
- **Q2 — Ollama Modelfile vs OpenAI API parameter precedence.** Which `Modelfile PARAMETER` settings the OpenAI shim respects vs ignores. Discovered in Unit 8 dry run; recorded in methodology doc.
- **Q3 — Run both `large-dense` and `large-moe` per host, or pick one?** Decided after Unit 8's first NVIDIA + Apple Metal runs. If the MoE cell adds genuine new signal (Ollama / LM Studio diverge from upstream), publish both; if not, drop `large-moe` from the matrix to bound runtime.
- **Q4 — Per-tool `llama.cpp` commit recording.** Best-effort capture in Unit 1's provenance module. Ollama embeds a version string; LM Studio's `lms version` is documented but tool-internal; `llama-server --version` is reliable. Recorded when discoverable, `null` when not.
- **Variance distribution per backend.** R140's 10 % / 25 % thresholds are estimates. After Unit 8's first runs, tune the thresholds in `scripts/bench/overhead/thresholds.json` and the variance gate constants in `render.py` based on observed noise.
- **Exact model picks per size class.** R128 suggests Qwen2.5 family but lets `LLAMASTASH_BENCH_MODELS_*` env vars override. Methodology doc records the canonical defaults; Unit 8 may swap if a pick has known bugs in one tool.

## High-Level Technical Design

> *This illustrates the intended approach and is directional guidance for review, not implementation specification. The implementing agent should treat it as context, not code to reproduce.*

### Orchestrator → Driver → Workload data flow

```
                              ┌──────────────────────────────────┐
                              │  scripts/bench/end-to-end/       │
                              │  orchestrator.py                 │
                              │                                  │
   provenance.py ─capture─────┤  for backend in detected:        │
   (host, GPU,                │    for model in {small, mid,     │
   tool versions)             │                 large_dense,     │
                              │                 large_moe}:      │
                              │      for tool in {llamastash,    │
                              │                   llamacpp,      │
                              │                   ollama,        │
                              │                   lmstudio}:     │
                              │        for mode in {defaults,    │
                              │                     normalized}: │
                              │          for workload in (4):    │
                              │            for rep in range(5):  │
                              │              driver.start(mode)  │
                              │              metrics =           │
                              │                workload(driver)  │
                              │              driver.stop()       │
                              │                                  │
                              └──────────────┬───────────────────┘
                                             │ Cell(reps=[…], summary)
                                             ▼
                              ┌──────────────────────────────────┐
                              │  schema.py validates → JSON      │
                              │  docs/benchmarks/runs/           │
                              │    <host-id>/<DATE>-<sha>.json   │
                              └──────────────┬───────────────────┘
                                             │
                                             ▼
                              ┌──────────────────────────────────┐
                              │  render.py reads all runs/*.json │
                              │  ├── variance gate (R140)        │
                              │  ├── markdown tables             │
                              │  └── SVG charts (matplotlib)     │
                              │  → docs/benchmarks/results-      │
                              │      <DATE>.md                   │
                              └──────────────────────────────────┘
```

### Driver Protocol shape (R136)

```python
# scripts/bench/end-to-end/drivers/base.py — directional only

class Mode(Enum):
    DEFAULTS = "defaults"
    NORMALIZED = "normalized"

@dataclass
class ModelHandle:
    name: str            # the identifier the tool uses to address the model
    source_path: Path    # the original GGUF on disk
    extra: dict          # tool-private state (Ollama's Modelfile path, etc.)

class Driver(Protocol):
    name: str            # "llamastash", "llamacpp", "ollama", "lmstudio"

    def version_string(self) -> str: ...
    def prepare_model(self, gguf_path: Path, mode: Mode) -> ModelHandle: ...
    def start(self, handle: ModelHandle, mode: Mode) -> str:  # returns http base URL
    def stop(self) -> None: ...
    def normalized_knobs_supported(self) -> set[str]:
        # subset of {ctx, n_gpu_layers, flash_attn, kv_cache_type, batch, ubatch}
        # that this tool actually exposes through start()
        ...
```

### Suite A two-tier threshold flow

```
overhead/orchestrator.py
  └─ raw_run  = run_one(raw_llama_server,  ctx=4096, NGL=99, port=P_raw)
  └─ stash_run = run_one(llamastash, LLAMASTASH_BENCH_DISABLE_DEFAULTS=1,
                         ctx=4096, NGL=99, port=P_stash)
  └─ assert_argv_equivalent(raw_run.argv, stash_run.argv, ignore=["--port"])
  └─ delta = compute_deltas(raw_run, stash_run)
  └─ tier  = classify(delta, thresholds.json)
              # tier ∈ {OK, ADVISORY, CATASTROPHIC}
  └─ write(docs/benchmarks/overhead/<host>/<date>-<sha>.json)
  └─ exit 1 if tier == CATASTROPHIC
     exit 0 with banner if tier == ADVISORY
     exit 0 silently if tier == OK
```

## Implementation Units

- [ ] **Unit 1: Bench harness skeleton + JSON schema + provenance**

**Goal:** Lay down the shared package skeleton, the R134 schema, and host/tool provenance capture. Establishes the entry shape both suites consume.

**Requirements:** R134, R135, R136 (entry/layout), R137 (tool-presence checks scaffolded), Q4 (best-effort version capture)

**Dependencies:** None

**Files:**
- Create: `scripts/bench/end-to-end/orchestrator.py`
- Create: `scripts/bench/end-to-end/run.sh`
- Create: `scripts/bench/end-to-end/schema.py`
- Create: `scripts/bench/end-to-end/provenance.py`
- Create: `scripts/bench/end-to-end/tests/__init__.py`
- Create: `scripts/bench/end-to-end/tests/test_schema.py`
- Create: `scripts/bench/end-to-end/tests/test_provenance.py`
- Modify: `scripts/requirements.txt` — add `httpx`, `pydantic>=2`, `matplotlib`
- Modify: `Makefile` — add `bench-end-to-end` target depending on `.venv/bin/python`

**Approach:**
- Pydantic models for `RunReport`, `Cell`, `Rep`, `Summary`, `Host`, `Provenance` matching the R134 example. `schema_version: Literal[1]`.
- `provenance.capture()` returns the full `Provenance` block: shells out for `llamastash --version`, `llama-server --version`, `ollama --version`, `lms version`; reads `/proc/cpuinfo` + `nvidia-smi --query-gpu=name` (or platform equivalents). Best-effort `llama.cpp` commit-string capture per Q4 — `llama-server --version` includes the upstream commit; Ollama's `ollama --version` includes its build SHA which maps to a vendored llama.cpp commit; LM Studio's `lms version` is recorded as-is. Each captured value is stored as a string; `null` when unavailable, never raises.
- `orchestrator.py` is the loop skeleton from the High-Level Design; concrete driver / workload calls land in Units 2 / 3 (start with stubs that raise `NotImplementedError`).
- `run.sh` mirrors `scripts/measure-overhead-band.sh`: env-var fallback for every flag, backend auto-detect, `.venv/bin/python` invocation, hostname-tagged output path.

**Patterns to follow:**
- `scripts/measure-overhead-band.sh` for the bash wrapper shape (lines 1–73 for arg parsing, 95–119 for venv check + mkdirs, 121–133 for hostname tagging).
- `Makefile` `.venv/bin/python` target (lines 28–36) — extend, don't fork.

**Test scenarios:**
- *Happy path:* `RunReport` round-trips through Pydantic (`model_dump_json()` then `model_validate_json()`) byte-equal for a fixture cell.
- *Happy path:* `provenance.capture()` populates all four tool version slots when the binaries are stubbed via PATH manipulation in the test.
- *Edge case:* `provenance.capture()` returns `null` for any tool whose binary is missing, never raises.
- *Edge case:* Schema rejects a `Cell` missing `tool`, `model`, or `mode` fields with a clear validation error.
- *Error path:* `RunReport` with `schema_version: 2` fails validation pointing at the bumped field.

**Verification:**
- `pytest scripts/bench/end-to-end/tests/` passes from the venv.
- `make bench-end-to-end -- --dry-run` exits zero and prints the planned matrix without running anything.

---

- [ ] **Unit 2: Per-tool drivers (llamastash, llamacpp, ollama, lmstudio)**

**Goal:** Implement the four `Driver` Protocol conformers. Each driver knows how to find its tool, declare its version, prepare a GGUF for loading, start it on a free port with defaults or normalized settings, and stop it cleanly.

**Requirements:** R126, R127, R128 (model loading paths per tool, including Ollama Modelfile import), R130 (defaults vs normalized per tool), R136 (Driver protocol), R137 (tool-not-found errors)

**Dependencies:** Unit 1

**Files:**
- Create: `scripts/bench/end-to-end/drivers/__init__.py`
- Create: `scripts/bench/end-to-end/drivers/base.py` — `Mode` enum, `ModelHandle`, `Driver` Protocol
- Create: `scripts/bench/end-to-end/drivers/llamacpp.py`
- Create: `scripts/bench/end-to-end/drivers/llamastash.py`
- Create: `scripts/bench/end-to-end/drivers/ollama.py`
- Create: `scripts/bench/end-to-end/drivers/lmstudio.py`
- Create: `scripts/bench/end-to-end/tests/test_drivers_smoke.py`
- Create: `scripts/bench/end-to-end/tests/test_ollama_import_sha.py`

**Approach:**
- Each driver implements `version_string()`, `prepare_model()`, `start()`, `stop()`, `normalized_knobs_supported()`.
- `llamacpp.py` and `llamastash.py` are thin: spawn the binary on a port, hit `/v1/models` for readiness, no model prep needed.
- `ollama.py` generates a `Modelfile` per GGUF path, runs `ollama create <bench-name> -f <Modelfile>`, captures the imported blob's SHA via `ollama show --modelfile`, asserts it matches the source GGUF SHA, then `ollama serve` (or detects running daemon) and uses the OpenAI shim. `stop()` runs `ollama rm <bench-name>` to keep the content-addressed store from accumulating.
- `lmstudio.py` uses `lms load <gguf-path>` + `lms server start`; in normalized mode passes `--context-length`, `--gpu` etc. for whatever the CLI supports, records the rest as `unfair_knobs` on the cell.
- `llamastash.py` sets `LLAMASTASH_BENCH_DISABLE_DEFAULTS=1` (depends on Unit 4) and uses `llamastash start --ctx … --n-gpu-layers …` with explicit knobs in normalized mode.
- `start()` polls `/v1/models` (or per-tool readiness equivalent) until 200 with a configurable timeout; on timeout, raises a structured error the orchestrator records as a skipped cell.
- All drivers honor `LLAMASTASH_BENCH_PORT_BASE` (default 18000) and increment to find a free port — avoids collisions across reps.

**Patterns to follow:**
- `scripts/measure-overhead-band.py` lines 200–300 (the llama-server spawn + `/health` poll loop) for the readiness probe pattern.
- `src/launch/binary.rs` for the "binary on PATH or fail with a helpful error" idiom — port to Python.

**Test scenarios:**
- *Happy path:* Each driver's `version_string()` returns a non-empty string when the tool is installed (skipped when not — `pytest.mark.skipif(shutil.which("ollama") is None)`).
- *Edge case:* `ollama.prepare_model()` raises `ToolNotFoundError` with a one-line install hint when `ollama` is missing from PATH.
- *Edge case:* `ollama.prepare_model()` verifies imported blob SHA matches source GGUF SHA and raises `ImportIntegrityError` if not. (Unit test — mock the `ollama show` output.)
- *Error path:* `start()` raises `ReadinessTimeoutError` after `LLAMASTASH_BENCH_READY_TIMEOUT_S` (default 180) if `/v1/models` never returns 200.
- *Error path:* `stop()` is idempotent — calling on an already-stopped driver returns silently.
- *Integration:* `llamastash.py` end-to-end with the `--features test-fixtures` `fake_llama_server` fixture: `prepare → start → version → stop` round trip on a `.gguf` fixture file, asserts the spawned argv contains `LLAMASTASH_BENCH_DISABLE_DEFAULTS=1` in the environment.

**Verification:**
- `pytest scripts/bench/end-to-end/tests/test_drivers_smoke.py` passes (with appropriate skipifs for tools the dev box lacks).
- For each installed tool, `python -m scripts.bench.end_to_end.drivers.<tool> --probe` prints the version string and exits zero.

---

- [ ] **Unit 3: Workload runners + metrics + GPU sampling (shared module refactor)**

**Goal:** The four workloads (R131), the metric calculators (R132), the fairness self-check (R141), and a refactor of the per-backend GPU samplers out of `scripts/measure-overhead-band.py` into a shared module.

**Requirements:** R131, R132, R141, plus Unit 1's schema must accept the metric outputs

**Dependencies:** Unit 1 (schema), Unit 2 (drivers to call against)

**Files:**
- Create: `scripts/bench/end-to-end/workloads.py` — `chat_turn`, `rag_prefill`, `agent_decode`, `parallel_4`
- Create: `scripts/bench/end-to-end/metrics.py` — TTFT, prompt-tps, decode-tps, e2e-latency, RSS, GPU mem aggregators
- Create: `scripts/bench/end-to-end/gpu_sampler.py` — extracted from `measure-overhead-band.py`
- Create: `scripts/bench/end-to-end/corpora/rag_prefill_8k.txt` — deterministic ~8k-token corpus, checked in
- Create: `scripts/bench/end-to-end/tests/test_workloads.py`
- Create: `scripts/bench/end-to-end/tests/test_metrics.py`
- Create: `scripts/bench/end-to-end/tests/test_gpu_sampler_compat.py`
- Modify: `scripts/measure-overhead-band.py` — import samplers from `scripts.bench.end_to_end.gpu_sampler` instead of defining them locally

**Approach:**
- Workloads take a `base_url: str` and `model_name: str`, return a `WorkloadResult` containing per-request timing and token streams. `parallel_4` uses `httpx.AsyncClient` for true concurrency.
- TTFT measured from request-send to first SSE chunk (R132). Decode tok/s averaged over `(total_tokens - 1) / (last_token_time - first_token_time)` per stream.
- `metrics.fairness_check()` compares token-ID sequences across drivers for the same (model, backend, normalized) cell. Per R141 (edited), same-backend comparison only; cross-backend logs but never fails.
- `gpu_sampler.py` exposes `sample_total(backend, gpu_id)` and `sample_per_process(backend, pid)` polymorphic over backend; `measure-overhead-band.py` imports both. The refactor's correctness is gated by `test_gpu_sampler_compat.py` which runs the existing measure script's output path against a golden fixture.
- `rag_prefill_8k.txt` is generated once (a deterministic combination of literature in the public domain or a synthesized repeat-pattern) and checked in; token count verified by the test against the canonical `tiktoken cl100k_base` encoder for reproducibility across environments.

**Patterns to follow:**
- `scripts/measure-overhead-band.py` lines 69–150 for the existing GPU-sampling shape — preserve the function signatures so the existing script keeps working post-refactor.
- `httpx`'s streaming response API for SSE parsing; `asyncio.gather()` for `parallel_4`.

**Test scenarios:**
- *Happy path:* `chat_turn` against a stubbed HTTP server returns a `WorkloadResult` with non-zero TTFT, decode-tps, and end-to-end latency.
- *Happy path:* `rag_prefill_8k.txt` tokenizes to between 7800 and 8200 tokens under `cl100k_base`.
- *Edge case:* `parallel_4` returns 4 results when all 4 streams succeed; one stream failing surfaces the failure on that result but does not abort the other 3.
- *Edge case:* `agent_decode` returns even when the server drops the connection after N tokens (records `truncated: true` instead of raising).
- *Error path:* `metrics.fairness_check()` flags `determinism_mismatch: true` when two same-backend drivers' token IDs diverge.
- *Integration:* `test_gpu_sampler_compat.py` runs `measure-overhead-band.py --dry-run` (a new flag, tiny add) against a golden JSON and asserts the output shape didn't change after the import refactor.

**Verification:**
- `pytest scripts/bench/end-to-end/tests/test_workloads.py scripts/bench/end-to-end/tests/test_metrics.py` passes.
- `python scripts/measure-overhead-band.py --help` still works after the import refactor.

---

- [ ] **Unit 4: LlamaStash env-var hook (`LLAMASTASH_BENCH_DISABLE_DEFAULTS`)**

**Goal:** When the env var is set, the launch resolver chain (`user > last_used > arch_defaults > builtin > model_default`) collapses to "user only" — presets, last-params, arch-defaults table, and built-in defaults all skip. The Rust hook the bench script depends on.

**Requirements:** R121 (Suite A needs LlamaStash and raw `llama-server` to produce byte-identical argv except for `--port`)

**Dependencies:** None (this is independent of the Python suites; it can land first)

**Files:**
- Modify: `src/launch/params.rs` — resolver chain reads `LLAMASTASH_BENCH_DISABLE_DEFAULTS`; when set, short-circuit to user knobs only
- Modify: `docs/usage.md §Environment variables` — document the new var (one line, marked "maintainer / bench-internal")
- Modify: `AGENTS.md §Common gotchas` — short note that this env var exists for the bench harness and should not be set in normal runs

**Approach:**
- Add an env-check at the top of the resolver function — a single early-return path that treats the resolved knobs as "exactly what the user typed, no layering."
- Standard Rust idiom: `std::env::var("LLAMASTASH_BENCH_DISABLE_DEFAULTS").ok().filter(|v| v == "1").is_some()`.
- The env var is read **at resolution time, not daemon start time** — bench can set it per-spawn without daemon restart.
- Document as "maintainer / bench-internal — never set in production runs" so users don't accidentally discover it as "the way to skip auto-tuning."

**Execution note:** Test-first. Add a failing test asserting the resolver skips arch-defaults when the env var is set, then add the env check.

**Patterns to follow:**
- Existing `LLAMASTASH_SOCKET`, `LLAMASTASH_STATE_DIR` env-var reads in `src/ipc/` and `src/state/` — follow the same `std::env::var` + `.ok()` filter pattern.
- Inline `#[cfg(test)] mod tests` per file (`AGENTS.md §Conventions`).

**Test scenarios:**
- *Happy path:* Env var unset → resolver applies the full chain (existing behavior unchanged). Assert via existing test in `params.rs`.
- *Happy path:* Env var set to `"1"` → resolver returns user knobs verbatim, arch-defaults table never consulted (test injects a mock resolver to confirm `lookup_defaults` is not called).
- *Edge case:* Env var set to any other value (`"0"`, `"true"`, `""`) → treated as unset; existing behavior. Documents the strict-`"1"` contract.
- *Edge case:* User-supplied knob plus the env var: only the user knob lands in the resolved `LaunchParams`; arch-default for the same knob is dropped.

**Verification:**
- `cargo test --features test-fixtures launch::params::tests` passes (new tests included).
- `cargo clippy --all-targets --features test-fixtures -- -D warnings` clean.
- `cargo fmt --all -- --check` clean.

---

- [ ] **Unit 5: Aggregation + rendering pipeline (variance gate, charts, dated results pages)**

**Goal:** Read all `docs/benchmarks/runs/**/*.json`, apply the R140 variance gate, render Markdown tables + SVG charts, write a dated results page, and update the benchmarks index.

**Requirements:** R138 (renderer), R139 (versioned dated pages), R140 (variance gate)

**Dependencies:** Unit 1 (schema to read against)

**Files:**
- Create: `scripts/bench/end-to-end/render.py`
- Create: `scripts/bench/end-to-end/charts.py`
- Create: `scripts/bench/end-to-end/tests/test_render.py`
- Create: `scripts/bench/end-to-end/tests/fixtures/sample_runs.json` — synthetic data covering OK / advisory / dropped cells

**Approach:**
- `render.py --date <YYYY-MM-DD>` discovers all run JSONs, validates each against schema v1, groups by `(model, backend)`, computes per-cell means + stddev, flags cells where stddev > 10 % of mean (R140), drops cells with stddev > 25 %.
- Markdown tables: one per (model, workload). Headline chart per model: decode tok/s in normalized mode across the four tools on the maintainer's primary backend.
- SVG charts via matplotlib's SVG backend (no JS). Saved alongside the results page; embedded via standard Markdown image syntax.
- `docs/benchmarks/results-<DATE>.md` written from a Jinja template with the methodology disclaimers from R142 carried forward.
- `docs/benchmarks/index.md` auto-prepended with a link to the new results page on each run.

**Patterns to follow:**
- `scripts/regenerate-benchmark-snapshot.py` for the "read source data, validate, write derived artifact" shape and the per-source-failure exit policy (treat schema mismatches as fatal, not silent skip).

**Test scenarios:**
- *Happy path:* Synthetic `sample_runs.json` with 3 clean cells renders a Markdown table containing the 3 cells and exactly 1 SVG chart per (model, workload) group.
- *Happy path:* Index `docs/benchmarks/index.md` gets a new line at the top pointing to the new results page; existing lines preserved in order.
- *Edge case:* A cell with stddev = 12 % of mean is **flagged** (renders with `±` and the stddev inline) but kept in the detail table; **excluded** from the headline chart.
- *Edge case:* A cell with stddev = 27 % of mean is **dropped** entirely with a "re-run needed" note in a footer section.
- *Error path:* Run JSON with `schema_version: 2` is rejected with a clear error pointing at the source file. Renderer exits non-zero rather than skipping silently.
- *Edge case:* Determinism mismatch in source JSON surfaces as a warning callout in the rendered page, not buried.

**Verification:**
- `pytest scripts/bench/end-to-end/tests/test_render.py` passes against the synthetic fixture.
- `python scripts/bench/end-to-end/render.py --date 2026-05-21 --dry-run` prints the planned outputs and exits zero without writing files.

---

- [ ] **Unit 6: Suite A overhead orchestrator + two-tier threshold harness**

**Goal:** The `llamastash start <model>` vs raw `llama-server` overhead suite. Reuses Units 1–5 modules; adds the orchestrator, thresholds JSON, bash wrapper, and run procedure documentation.

**Requirements:** R121–R125

**Dependencies:** Units 1, 2, 3 (driver + workload modules); Unit 4 (env var the harness sets); Unit 5 not strictly required (Suite A is single-cell, can format in-place)

**Files:**
- Create: `scripts/bench/overhead/orchestrator.py`
- Create: `scripts/bench/overhead/run.sh`
- Create: `scripts/bench/overhead/thresholds.json` — R123 two-tier defaults
- Create: `scripts/bench/overhead/tests/test_argv_equivalence.py`
- Create: `scripts/bench/overhead/tests/test_threshold_classifier.py`
- Modify: `Makefile` — add `bench-overhead` target

**Approach:**
- Composes `drivers/llamacpp.py` and `drivers/llamastash.py` (the latter with the env var set per Unit 4), runs the `chat_turn` workload 5 times against each (1 warmup + 4 measured), compares the resolved argv after stripping `--port`, computes deltas, classifies against `thresholds.json`, writes output JSON, exits per the threshold classification (`0` ok, `0` with banner for advisory, `1` for catastrophic).
- `thresholds.json` defaults from R123: `catastrophic: { ttft_ms_delta: 200, decode_tps_delta_pct: 2.0, daemon_idle_rss_mb: 64 }`, `advisory: { ttft_ms_delta: 30, decode_tps_delta_pct: 0.5, daemon_idle_rss_mb: 48 }`. Per-backend overrides via `thresholds.json` nested keys.
- `run.sh` mirrors `scripts/measure-overhead-band.sh`: env-var fallback for every flag, model auto-fetch into `.cache/`, hostname-tagged output.
- Output path: `docs/benchmarks/overhead/<host-id>/<YYYY-MM-DD>-<commit-sha>.json` per R125.

**Patterns to follow:**
- `scripts/measure-overhead-band.sh` (full file) as the wrapper template.
- `tests/fixtures/fake_llama_server.rs` for the smoke-test fixture (`--features test-fixtures` required when smoke-testing the harness end-to-end without a real GPU).

**Test scenarios:**
- *Happy path:* `assert_argv_equivalent()` returns true when two argv lists differ only in `--port <N>`.
- *Edge case:* `assert_argv_equivalent()` returns false (with a clear diff message) when an unexpected flag is present in one side. Tests cover: extra `-c`, missing `--n-gpu-layers`, reordered flags (order must match — the assertion is strict).
- *Edge case:* Threshold classifier returns `CATASTROPHIC` when TTFT delta = 201 ms; `ADVISORY` at 31 ms; `OK` at 29 ms. Boundary values tested explicitly.
- *Edge case:* Per-backend threshold override in `thresholds.json` applied correctly — `metal.advisory.ttft_ms_delta = 50` overrides the global default of 30.
- *Integration:* End-to-end smoke against `fake_llama_server` (gated on `--features test-fixtures`) — 1 rep, asserts the output JSON is schema-valid and a delta is computed (the *values* aren't meaningful in the fake-server path, just the wiring).

**Verification:**
- `pytest scripts/bench/overhead/tests/` passes.
- `make bench-overhead -- --dry-run` prints the planned raw and llamastash argv and exits zero.
- `make bench-overhead` on the maintainer's box (real `llama-server` + a real 1.5B GGUF) produces a JSON and a tier classification.

---

- [ ] **Unit 7: Methodology doc + benchmarks index + README "Benchmarks" section**

**Goal:** The unconditional-ship docs that explain what the harness does, how to re-run it, and the per-tool fairness notes. Lands before any results page so the methodology has a stable home from day one.

**Requirements:** R142 (methodology doc + benchmarks index), R143 (README section), R144 (methodology doc unconditional)

**Dependencies:** None (pure docs; can land in parallel with Units 1–6)

**Files:**
- Create: `docs/benchmarks/index.md` — chronological list of results pages
- Create: `docs/benchmarks/methodology.md` — the contract
- Create: `docs/benchmarks/runs/README.md` — per-host runs subdirectory pointer
- Create: `docs/benchmarks/overhead/README.md` — Suite A's output subdirectory pointer
- Modify: `README.md` — add "Benchmarks" section between "Features" and "Install", with one paragraph + link to `docs/benchmarks/index.md` (no headline chart yet — that lands with Unit 8)
- Modify: `AGENTS.md §Protected artifacts` — add `docs/benchmarks/*` to the protected list so future cleanups don't strip historical results pages

**Approach:**
- Methodology doc covers: tool versions captured per run, the matched-pair settings policy (R130 with concrete examples of "fair" vs "unfair" knob), per-tool fairness notes (LM Studio's `lms` limitations from Q1 dry-run when known, Ollama's Modelfile/OpenAI-shim precedence from Q2), variance gate semantics, the cross-backend determinism caveat (R141 edited), the "first-party benchmark, here's how to verify" disclaimer.
- `index.md` starts with one entry: "*(no results page yet — first run lands with Unit 8)*". Updated by the renderer (Unit 5) on each new run.
- README section: one paragraph, no chart yet (chart appears post-Unit-8 when there are actual numbers; explicit "TBD" until then).

**Patterns to follow:**
- `docs/usage.md` for tone and depth.
- `docs/architecture.md` for the "stable user-facing summary" register.

**Test scenarios:**
- *Test expectation: none — pure documentation, no behavior.* Human review pass against the brainstorm's R142–R144 covers the doc's correctness.

**Verification:**
- Manually re-read `docs/benchmarks/methodology.md` against the requirements doc; every R130 / R131 / R132 / R140 / R141 element has a corresponding methodology section.
- `README.md` renders cleanly in `gh pr view --web` preview after the Benchmarks section lands.
- Links in `index.md` and `methodology.md` resolve (no broken relative paths).

---

- [ ] **Unit 8: First end-to-end results run + dated results page (resolves Q1, Q2, Q3, Q6)**

**Goal:** Run Suite B on the maintainer's primary backend(s), commit the per-host run JSON(s), generate the first dated `results-<DATE>.md`, and update the methodology doc with the actual answers to Q1 / Q2 / Q6.

**Requirements:** R128 (model picks finalized), R129 (which backends covered in first run), R134 (output schema in action), R138 (renderer produces real artifact), R142 (results page lives in docs/benchmarks/)

**Dependencies:** Units 1, 2, 3, 5, 7. Unit 4 not strictly required for Suite B (Suite B uses `llamastash start` with explicit normalized knobs, env var only matters for Suite A's byte-equal argv assertion).

**Files:**
- Create: `docs/benchmarks/runs/<host-id>/2026-XX-XX-<sha>.json` — one or more per backend covered
- Create: `docs/benchmarks/results-2026-XX-XX.md` — first published results page
- Create: `docs/benchmarks/results-2026-XX-XX/` — SVG charts subdirectory
- Modify: `docs/benchmarks/methodology.md` — add Q1, Q2, Q6 answers discovered during this run (LM Studio's actual normalization ceiling; Ollama's Modelfile vs API precedence map; the TTFT-with-vs-without lazy-load distinction)
- Modify: `docs/benchmarks/index.md` — auto-updated by the renderer; verify the new entry lands correctly
- Modify: `README.md` — replace the "TBD" headline chart with the real SVG
- Modify: `TODO.md` — close Q1, Q2, Q6 entries (if they were tracked there); add Q3 follow-up (large-dense vs large-moe per host) if the answer needs more data

**Approach:**
- Run on the maintainer's primary GPU backend first (Apple Metal or NVIDIA, whichever is local).
- Resolve Q3 inline: try both `large-dense` and `large-moe`; if one cell adds no new signal (e.g. all tools converge within variance), drop it from the matrix and document the decision in the methodology doc.
- Capture wall-clock for the run; record in the results page footer so future readers know the runtime budget.
- The methodology doc gets a new "Tool-specific fairness notes" subsection with the discovered Q1 / Q2 answers.

**Patterns to follow:**
- The dated results-page filename mirrors `docs/plans/YYYY-MM-DD-NNN-…-plan.md` for searchability.

**Test scenarios:**
- *Test expectation: none — execution + curation. Validated by the renderer's existing tests (Unit 5) plus human review of the published page.*
- Sanity check before landing: the published page's headline numbers match the source JSON (visual diff during review).

**Verification:**
- `docs/benchmarks/results-2026-XX-XX.md` renders the headline chart, the per-cell tables, the methodology link, and the variance footnotes.
- `docs/benchmarks/methodology.md`'s Q1 / Q2 / Q6 sections are no longer placeholder.
- README's "Benchmarks" section now shows the real headline SVG.

---

- [ ] **Unit 9 (conditional): Launch-ready blog post (gated on separate review)**

**Goal:** A long-form narrative post drafted from Unit 8's numbers. Shipped only after a separate review confirms there's a real surprise / win / honest finding worth sharing — per R144.

**Requirements:** R144

**Dependencies:** Unit 8 must be complete and reviewed.

**Files:**
- Create: `docs/benchmarks/2026-XX-XX-vs-ollama-lm-studio.md` — draft state, no `status: ready` until separate review
- Modify: `CHANGELOG.md` — entry under `[Unreleased]` only when the post actually publishes, not when it's drafted

**Approach:**
- Lead with whatever surprise the numbers produced — Suite A's near-zero overhead, an unexpected Suite B finding (parallel-throughput delta, MoE coverage gap, defaults-vs-normalized gap, etc.), or the methodology caveats themselves if the numbers are unremarkable.
- Methodology caveats stated up front, not buried in a footnote.
- Links: (a) `docs/benchmarks/results-<DATE>.md`, (b) `docs/benchmarks/methodology.md`, (c) `scripts/bench/` so any reader can re-run.
- Suitable for cross-posting to HN / Reddit / X if the surprise is real; otherwise, becomes a different artifact (e.g. "what we learned about defaults vs normalized" — still ships, different framing).

**Patterns to follow:**
- The brainstorm doc itself (`docs/brainstorms/2026-05-21-benchmark-harness-requirements.md`) for tone — confident about the architectural angle, honest about the comparison's limits.

**Test scenarios:**
- *Test expectation: none — pure narrative.* The gate is human editorial review, not code.

**Verification:**
- Separate review pass (the maintainer or a trusted reviewer) confirms the post has a real headline, not a manufactured one.
- If the review says "no real surprise to share," the file stays in draft and the unit is recorded as "deferred" rather than "complete" — no marketing-pressure ship.

## System-Wide Impact

- **Interaction graph:** One new env-var read in `src/launch/params.rs` (Unit 4). No changes to IPC, supervisor, daemon lifecycle, TUI, CLI surface, or wire protocols. The bench harness is a sibling tool, not a daemon or library consumer.
- **Error propagation:** All harness errors stay inside Python. The Rust env-var hook is a single early-return; nothing in the launcher path changes its error model. Bench tool-not-found errors surface before any model spawn and exit non-zero with a one-line install hint.
- **State lifecycle risks:** Ollama imports each tested GGUF into its content-addressed store; without cleanup, the store grows by ~`(size of all bench models)` per run. The Ollama driver's `stop()` runs `ollama rm` per imported model to bound this. LM Studio's cache is read-through to the GGUF path and doesn't accumulate. No LlamaStash state changes — `LLAMASTASH_BENCH_DISABLE_DEFAULTS=1` is ephemeral per-process.
- **API surface parity:** None. The harness does not add CLI subcommands, IPC methods, or config keys. The new env var is documented as maintainer-internal and not surfaced in `--help`.
- **Integration coverage:** Unit 6's smoke test (Suite A against `fake_llama_server`) is the only cross-layer test. Real performance numbers come from human-run, not CI. Suite A's two-tier classifier and the renderer's variance gate are the primary correctness guards.
- **Unchanged invariants:** The loopback-only / same-UID daemon contract (`AGENTS.md §Scope boundaries`) is untouched. The `list_json` / `status_json` / `--json` agent surfaces are untouched. The `(architecture, gpu_backend) → TypedKnobs` defaults table is untouched in its content — the new env var only short-circuits the resolver's *consultation* of the table, not the table itself. Existing `LLAMASTASH_*` env vars work exactly as before.

## Risks & Dependencies

| Risk | Mitigation |
|------|------------|
| **Tool API drift** — Ollama / LM Studio bump a major version and break a driver. | R139 dated-results policy. Each results page records exact versions; rerun cycles publish a new page rather than overwriting. Driver-level breakage surfaces immediately on the next maintainer run, not silently in production. |
| **Disk consumption** — `(4 sizes × ~4 GB) × tool import duplication` can hit 50+ GB per backend host. | Methodology doc documents the disk budget. Ollama driver runs `ollama rm` per imported model in `stop()`. The `LLAMASTASH_BENCH_KEEP_IMPORTS=1` env var skips the cleanup for debugging. |
| **Sampler accuracy drift across backends** — GPU samplers return different units / refresh rates across `nvidia-smi`, `rocm-smi`, `powermetrics`, `metal-tracker`. | Variance gate (R140) catches noisy cells. The samplers were already exercised by `scripts/measure-overhead-band.py`; Unit 3's refactor is gated by `test_gpu_sampler_compat.py` against a golden fixture. |
| **Normalized-mode fairness escape hatches** (Q1, Q2). | Knobs the tool refuses to expose are recorded as `unfair_knob`; the cell ships with a warning. The methodology doc lists the actual per-tool ceiling after Unit 8's dry run. |
| **Ollama's import overhead masks startup metrics** — first-request lazy-load is part of TTFT or not? (Q6) | Report both `ttft_ms_first_request` and `ttft_ms_post_load`. The renderer's headline TTFT is the second (engine-comparable); the first is in the detail table. |
| **First-party benchmark credibility** — "they benchmark themselves." | Repeatability contract (R135): any reader can re-run on their hardware with `./scripts/bench/end-to-end/run.sh`. JSONs are raw, charts are deterministic SVG, the methodology doc disclaims the conflicts of interest up front. |
| **Maintenance commitment** (R139). | R125 already caps this — no CI, no automated rerun expectation. Maintainer reruns happen at their pace; stale results pages don't block product velocity. |
| **Refactoring `scripts/measure-overhead-band.py` mid-way through a snapshot cycle.** | Unit 3's `test_gpu_sampler_compat.py` runs the existing `measure-overhead-band.py` against a golden output fixture before merging. Failure means revert; the refactor doesn't ship until equivalence is proven. |

## Phased Delivery

### Phase 1 — Foundations (parallelizable)
- **Unit 1** (skeleton + schema + provenance)
- **Unit 2** (drivers) — depends on Unit 1
- **Unit 3** (workloads + metrics + sampler refactor) — depends on Unit 1
- **Unit 4** (Rust env-var hook) — independent; can land first
- **Unit 7** (docs prep) — independent; can land first

### Phase 2 — Suite A (overhead regression check)
- **Unit 6** (overhead orchestrator) — depends on Units 1, 2, 3, 4

### Phase 3 — Rendering pipeline
- **Unit 5** (renderer) — depends on Unit 1; can ship before Suite B has real data using synthetic fixtures

### Phase 4 — First publish
- **Unit 8** (first end-to-end results run) — depends on Units 1, 2, 3, 5, 7

### Phase 5 — Narrative (conditional)
- **Unit 9** (blog post) — depends on Unit 8 plus separate review

## Documentation Plan

- `README.md` — new "Benchmarks" section (Unit 7), headline SVG updated (Unit 8)
- `docs/benchmarks/methodology.md` — new (Unit 7), updated with Q1/Q2/Q6 answers (Unit 8)
- `docs/benchmarks/index.md` — new (Unit 7), auto-updated per renderer run
- `docs/benchmarks/results-<DATE>.md` — first published page (Unit 8); future runs add dated siblings
- `docs/benchmarks/runs/<host-id>/*.json` — raw run data (Units 6 + 8)
- `docs/benchmarks/overhead/<host-id>/*.json` — Suite A run data (Unit 6)
- `docs/usage.md §Environment variables` — `LLAMASTASH_BENCH_DISABLE_DEFAULTS` documented (Unit 4)
- `AGENTS.md §Common gotchas` — mention the bench env var exists; **§Protected artifacts** — add `docs/benchmarks/*`
- `CHANGELOG.md [Unreleased]` — single user-facing line ("docs: published cross-tool benchmarks (#…)") when Unit 8 lands; blog post entry only if Unit 9 ships
- `TODO.md` — track Q3 (large-dense vs large-moe per host) if Unit 8's first run doesn't decisively answer it

## Operational / Rollout Notes

- No production rollout — both suites are maintainer-run. No feature flag, no migration, no monitoring.
- The env var (Unit 4) is documented as bench-internal in `docs/usage.md` and `AGENTS.md` to prevent accidental discovery as "the way to launch fast."
- Suite A is expected to be run before each release tag; Unit 6's docs include a one-line addendum to the release runbook (`docs/runbooks/release-0.0.1-bootstrap.md`) noting the trigger.
- Suite B is run on the maintainer's cadence; results pages stay forever, no rotation. Index page (Unit 7) lists chronologically.

## Sources & References

- **Origin document:** [docs/brainstorms/2026-05-21-benchmark-harness-requirements.md](../brainstorms/2026-05-21-benchmark-harness-requirements.md)
- **Existing harness precedent:** `scripts/measure-overhead-band.py`, `scripts/measure-overhead-band.sh`, `scripts/regenerate-benchmark-snapshot.py`
- **VRAM overhead spike (related but different harness):** `docs/spikes/2026-05-19-vram-overhead-band.md`
- **Launch resolver chain (hook site for Unit 4):** `src/launch/params.rs` line 142, `src/launch/defaults_table.rs`
- **Project scope contract:** `AGENTS.md §Scope boundaries`, `AGENTS.md §Docs stay in sync with code`
- **Test fixture for harness smoke tests:** `tests/fixtures/fake_llama_server.rs` (requires `--features test-fixtures`)
