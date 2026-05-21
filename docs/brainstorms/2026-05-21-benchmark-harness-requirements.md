---
date: 2026-05-21
topic: benchmark-harness
---

# Cross-Tool Benchmark Harness (LlamaStash vs Ollama vs LM Studio)

> Companion to [`docs/brainstorms/llamatui-requirements.md`](./llamatui-requirements.md) (origin positioning: README "Why") and [`docs/spikes/2026-05-19-vram-overhead-band.md`](../spikes/2026-05-19-vram-overhead-band.md) (precedent for cross-backend measurement harnesses). IDs continue from R120 to stay globally unambiguous.

## Problem Frame

The README's "Why" leads with **"Heavy abstractions (Ollama, LM Studio) hide llama.cpp; raw `llama-server` use is tedious. LlamaStash is a fast, transparent launcher…"** This is the founding positioning claim and currently has zero quantitative backing. Two architecturally-distinct questions hide inside it:

1. **Does LlamaStash in fact add ~0 inference overhead vs running `llama-server` directly?** By construction it should — LlamaStash spawns the upstream `llama-server` binary unmodified, with all process-survival, IPC, and supervisor work happening out-of-band of the inference hot path. But "should" is an architectural intent, not a measurement. Without a regression gate, a future change to the supervisor, IPC dispatch, or sampler loop could quietly add latency or memory pressure that nothing catches.
2. **How does LlamaStash-as-shipped compare to Ollama / LM Studio on the same model + hardware?** This is the question prospective users actually ask. The honest answer is "the engines underneath are different, so end-to-end numbers reflect the engine + the tool's defaults + the tool's HTTP server, not LlamaStash alone." A defensible answer requires methodology, not vibes — and the methodology needs to make the conflation visible so readers can interpret the numbers correctly.

These are different artifacts with different audiences, but they share infrastructure: process driving, OpenAI-compatible HTTP probing, GPU sampling, JSON output. Bundling them into one harness with two suites avoids duplicate scaffolding while keeping the stories clearly separated. The existing `scripts/measure-overhead-band.py` is precedent for this shape — a Python harness driving `llama-server` across the full backend matrix, emitting per-host JSON. The new harness extends that pattern to multi-tool comparison.

**Audience:**
- Primary (Suite A): LlamaStash maintainers. Need an overhead regression check the maintainer runs before tagging a release (and after substantial daemon / launch / IPC / supervisor changes), to catch the case where the launcher / supervisor / IPC quietly grows fat. CI gating was considered and explicitly rejected (R125) — the signal is meaningful only when the maintainer is paying attention to it anyway, and the infra cost of self-hosted GPU CI doesn't pay for itself on a small project.
- Secondary (Suite B): prospective users evaluating LlamaStash vs Ollama / LM Studio on real hardware. Need a methodology they can replicate and numbers they can verify, not a marketing chart.
- Tertiary (Suite B): HN / Reddit / X readers who have never heard of LlamaStash. Need a launch-ready post that puts the comparison in front of them and links to the methodology with one click.

## Requirements

**Suite A — Overhead vs raw `llama-server`**

- **R121.** The overhead suite compares two invocations of the same `llama-server` binary on the same machine, same model file, same prompt: (1) raw `llama-server` started directly with `--model … --ctx 4096 --port <P_raw> --n-gpu-layers <NGL>`, and (2) `llamastash start <model>` configured to produce the same effective command on a different port `<P_stash>`. The runs are sequential, not concurrent, so the ports differ only to keep the two configurations independently restartable; the harness asserts that the resolved argv match **after the `--port` flag is stripped from both sides**. The LlamaStash run disables presets, last-params, and the built-in arch-defaults table for the duration of the suite (new env var `LLAMASTASH_BENCH_DISABLE_DEFAULTS=1` — a small new surface that this suite introduces and that the bench script sets; it's never set in normal use) so the two invocations agree on every other arg.
- **R122.** Metrics: **cold-launch TTFT** (wall-clock from process spawn until the first streamed token from `/v1/chat/completions`), **steady-state decode tok/s** (averaged over a 256-token decode, after one warmup request), **daemon idle RSS** (LlamaStash only, sampled after the model is Ready and before any inference), **daemon RSS during inference** (peak during the 256-token decode), and **supervisor sampler CPU cost** (`%CPU` of the daemon process, mean over a 30 s idle period with one Ready model).
- **R123.** **Two-tier thresholds**, tracked in `scripts/bench/overhead/thresholds.json` and applied by the harness when it prints its results table:
  - **Catastrophic (hard-fail)** — cold-launch TTFT delta > 200 ms vs raw, steady-state tok/s outside ±2 % of raw, daemon idle RSS > 64 MB. The script exits non-zero. Intent: catches shipping disasters when the maintainer remembers to run it.
  - **Advisory (warn-only)** — TTFT delta > 30 ms, steady-state tok/s outside ±0.5 %, daemon idle RSS > 48 MB. The script highlights the row but exits zero. Intent: tracks the "~0 overhead" architectural claim closely so drift surfaces early without crying wolf on system noise.
  Thresholds are tunable per backend via the JSON config; tuning happens in the same change that legitimately moves the floor, with a comment explaining why. Both tiers are reported in the JSON output (R134-style provenance applies here too).
- **R124.** Harness lives at `scripts/bench/overhead/`. Single workload (chat-turn from Suite B) on a single 1.5B-class model (the same one bundled or first-fetched by `init --recommended` on CPU, so cold runs don't depend on network). Entry point `scripts/bench/overhead/run.sh`; emits a JSON file to `docs/benchmarks/overhead/<host-id>/<YYYY-MM-DD>-<commit-sha>.json`. Both `Makefile` (`make bench-overhead`) and direct shell invocation are supported.
- **R125.** **Run policy: maintainer-only, no CI.** The overhead suite is not wired into GitHub Actions; it runs on the maintainer's hardware on demand (typically before tagging a release, and after substantial changes to `src/daemon/`, `src/launch/`, `src/ipc/`, or `src/supervisor/`). A short procedure in `docs/benchmarks/methodology.md` documents the trigger criteria so future maintainers know when to run it. Outputs land in `docs/benchmarks/overhead/<host-id>/` and are committed alongside the change that motivated the rerun, so the record of "this release didn't regress" is durable. Rationale: GitHub-hosted runners have no GPU and unstable per-job perf, self-hosted infra is a real recurring cost, and the regression-gate signal is meaningful only when the maintainer is paying attention to it anyway. The cross-tool suite (R126+) is likewise manual.

**Suite B — Cross-tool comparison**

- **R126.** Tools under test (v1, fixed): **LlamaStash** (current release), **Ollama** (current `ollama` release on PATH), **LM Studio** (current GA, driven via the `lms` CLI + its local OpenAI server), and **raw `llama-server`** (the same llama.cpp version that LlamaStash spawns — recorded in provenance, not pinned by the harness). The raw-`llama-server` column is the reference that lets readers separate "engine difference" from "tool wrapper difference." Adding tools (Jan, GPT4All, llamafile, KoboldCpp) is explicitly out of v1; reopen after the v1 harness ships.
- **R127.** Each tool is driven exclusively via its OpenAI-compatible HTTP endpoint (`/v1/chat/completions`, plus `/v1/embeddings` if we add an embedding workload in a follow-up). This is the user-facing contract for all four tools and the fairest cross-tool API. No native-only protocol shortcuts (no `ollama generate` Go client, no `lms` direct inference call). The harness records the per-tool endpoint URL and request payload in provenance so reviewers can verify nothing tool-specific snuck into the wire format.
- **R128.** Three model classes, each a distinct shape:
  - **small** — 1.5B-class dense, Q4_K_M GGUF (e.g. Qwen2.5-1.5B-Instruct).
  - **mid** — 7B-class dense, Q4_K_M GGUF (e.g. Qwen2.5-7B-Instruct).
  - **large** — picked to maximize differentiation, **not** to be the biggest possible: a 14B-class dense (e.g. Qwen2.5-14B-Instruct, Q4_K_M) **and** a 30B-A3B-class MoE (e.g. Qwen3-30B-A3B, Q4_K_M) where the matrix allows it. The MoE is where Ollama / LM Studio historically diverge from upstream llama.cpp; the 14B dense keeps "large" runnable on more configs. The harness treats these as two named cells (`large-dense`, `large-moe`); see Q3.

  All tools load the **same GGUF file on disk**: Ollama ingests it via a generated `Modelfile FROM <abs-path>` (Ollama then copies the file into its content-addressed store on first import — a one-time per-model disk + time cost handled by the Ollama driver's `prepare_model` step in R136; the harness verifies the imported model SHA matches the source GGUF SHA in provenance); LM Studio's `lms` loads the file by path; LlamaStash + raw `llama-server` use it directly. The harness picks suggested defaults documented in `docs/benchmarks/methodology.md`; pin overrides via `LLAMASTASH_BENCH_MODELS_SMALL`, `_MID`, `_LARGE_DENSE`, `_LARGE_MOE` env vars.
- **R129.** Hardware matrix: **NVIDIA CUDA**, **Apple Metal**, **AMD/ROCm (Linux)**, **CPU-only**. The matrix is **non-rectangular** — the large model is excluded from CPU-only and from any backend with < 20 GB VRAM headroom (the harness detects this via the existing recommender path and skips the cell with a documented `skipped: insufficient_vram` reason in the output JSON, rather than running and failing). The harness ships matching numbers for whatever subset of the matrix a maintainer can actually run; community contributions fill the rest (R134 schema makes merging trivial).
- **R130.** **Settings pair per cell** — every (tool, model, hardware) cell runs twice:
  - **Defaults mode.** Each tool started with no overrides: `ollama run <model>`, LM Studio's default load profile, `llamastash start <model>` with no preset / last-params / overrides, `llama-server --model <path> --ctx 4096 --port <P>`. Captures the experience a new user gets out of the box.
  - **Normalized mode.** Target across all four tools: `ctx = 4096` (raised to `8192` for the `rag-prefill` workload — see R131), `n_gpu_layers = all`, `flash_attn = on` (where backend supports — AMD/ROCm flash-attn coverage is uneven per `AGENTS.md`; on backends where one tool can't enable it, normalized mode runs `flash_attn = off` everywhere for that backend so the comparison stays fair), KV cache `F16`, `batch = 512`, `ubatch = 512`. Sampling: `temperature = 0`, `seed = 42`, `top_k = 1` (greedy decode). Any setting a tool refuses to expose is recorded as `"unfair_knob": "<name>"` on the cell and surfaced as a warning in the rendered report; the run still proceeds. Documented per-tool fairness notes in `docs/benchmarks/methodology.md`.
- **R131.** Four workloads per cell, all driven through `/v1/chat/completions`:
  - **chat-turn** — ~50-token system prompt + 1 user turn ("Write a haiku about <fixed topic>"), `max_tokens = 128`, single request, streaming on. Reports TTFT + decode tok/s.
  - **rag-prefill** — ~8 000-token synthetic prefilled context (deterministic; checked into the harness as a fixed corpus) + short instruction, `max_tokens = 128`, streaming on. Reports prompt-processing tok/s + TTFT.
  - **agent-decode** — ~50-token prompt, `max_tokens = 2048`, streaming on. Pure decode-dominated. Reports decode tok/s + end-to-end latency.
  - **parallel-4** — 4 concurrent chat-turn requests, separate HTTP connections, fired within a 50 ms window. Reports per-request decode tok/s, aggregate tokens-per-second across all 4 streams, and the 4-stream latency stddev (server-scheduling fairness signal).
- **R132.** Metrics per (cell, workload): **TTFT** (ms), **prompt-processing tok/s**, **decode tok/s**, **end-to-end latency** (ms), **peak host-process RSS** (bytes), **peak GPU memory delta** (bytes; using the same per-backend samplers as `scripts/measure-overhead-band.py::_sample_*`). Metrics that don't apply to a given workload (e.g. prompt-processing tok/s on agent-decode) are omitted, not zero-filled.
- **R133.** **Loop structure.** Outer loop: (tool, model, hardware, mode). Inner loop: workload. For each workload, the tool process is **fully torn down and restarted before each of 5 reps** so cold-start metrics (TTFT especially) reflect a real cold path on every measurement. First rep is a warmup and is discarded; the remaining 4 contribute to mean and standard deviation. Workloads within a cell don't share warm state — each (workload, rep) is its own cold start. Between modes (defaults → normalized) the tool is restarted, which the outer loop already guarantees. Total runs per cell: 4 workloads × 5 reps = 20.
- **R134.** Output schema, versioned (`"schema_version": 1`). One JSON per (host, run) at `docs/benchmarks/runs/<host-id>/<YYYY-MM-DD>-<commit-sha>.json`:
  ```json
  {
    "schema_version": 1,
    "run_id": "2026-05-21-abc1234",
    "host": { "cpu_model": "...", "gpu_model": "...", "gpu_backend": "nvidia", "ram_total_bytes": …, "os": "Linux 6.…", "governor": "performance" },
    "provenance": {
      "llamastash_version": "0.3.0-…", "llamacpp_version": "b4012",
      "ollama_version": "0.x.y", "lmstudio_version": "0.x.y",
      "model_files": { "small": "<sha256>:<path>", "mid": …, "large": … }
    },
    "cells": [
      { "tool": "llamastash", "model": "mid", "mode": "normalized", "workload": "chat-turn",
        "reps": [ { "ttft_ms": …, "decode_tps": …, … }, … ],
        "summary": { "ttft_ms": { "mean": …, "stddev": … }, "decode_tps": { "mean": …, "stddev": … }, … },
        "unfair_knobs": [], "skipped": null }
    ]
  }
  ```
- **R135.** **Repeatability contract.** Any user on supported hardware runs `./scripts/bench/end-to-end/run.sh --backend nvidia --models small,mid` and produces a JSON of the R134 shape. Submitting it as a PR against `docs/benchmarks/runs/<their-host-id>/` plus a one-paragraph `README.md` describing their host adds their numbers to the published comparison. The harness must not require any LlamaStash-org credentials, paid model accounts, or non-redistributable assets to run.
- **R136.** Harness is Python + `uv` (matching `scripts/measure-overhead-band.py` and `scripts/regenerate-benchmark-snapshot.py`). Single entry `scripts/bench/end-to-end/run.sh` that detects backend or accepts `--backend {nvidia,metal,amd,cpu}`. Per-tool drivers under `scripts/bench/end-to-end/drivers/{llamastash,llamacpp,ollama,lmstudio}.py`, each implementing a minimal interface:
  ```python
  class Driver:
      def version_string(self) -> str: ...
      def prepare_model(self, gguf_path: Path, mode: Mode) -> ModelHandle: ...
      def start(self, handle: ModelHandle, mode: Mode) -> str:  # returns http base URL
      def stop(self) -> None: ...
  ```
  Adding a fifth tool in a follow-up means writing one new driver — the orchestrator, workloads, and metrics stay tool-agnostic.

**Common infrastructure**

- **R137.** **Tool install is out of scope.** The harness verifies each tool is on PATH (or at a configurable path) and exits early with a clear "ollama not found — install via …" message otherwise. Documented prerequisites in `docs/benchmarks/methodology.md` for each tool. Both suites run on the maintainer's box per R125; the cross-tool install matrix is a human concern, never a CI concern.
- **R138.** **Aggregation + rendering.** `scripts/bench/end-to-end/render.py` reads all `docs/benchmarks/runs/**/*.json`, validates against `schema_version: 1`, and emits a dated Markdown results page (`docs/benchmarks/results-<YYYY-MM-DD>.md`) with: per-cell tables, headline charts as static SVG (matplotlib's SVG export — no JS, no interactive widgets in v1), and a "raw data" section listing every contributing JSON. The latest results page is linked from `README.md` under a new "Benchmarks" section.
- **R139.** **Version pinning + versioned results.** Each results page records the exact tool versions used. When any tool's major version changes, the maintainer reruns and publishes a new dated `results-<DATE>.md` — the old one stays as a historical record (no overwrites). `docs/benchmarks/index.md` is the index of all dated results pages, newest first.
- **R140.** **Variance gate.** If stddev across the 4 non-warmup reps exceeds 10 % of mean for any reported metric on a given cell, that cell is flagged in the rendered tables (with a `±` symbol and the stddev shown inline) and **excluded from the headline chart** (the cell still appears in the raw JSON and in the detail tables). Goal: refuse to publish noisy numbers as if they were precise. Cells that exceed 25 % stddev are dropped from the published report entirely with a "re-run needed" note, not silently averaged.
- **R141.** **Fairness self-check.** In normalized mode (`temperature = 0`, `seed = 42`, `top_k = 1`), all four tools running the same model on the **same hardware backend** should produce **token-ID-identical generations** (compare token IDs, not detokenized strings — different tokenizers' whitespace handling is not the signal we care about). Cross-backend comparison is intentionally **not** asserted: floating-point determinism across CUDA / Metal / ROCm / CPU is not guaranteed even within one engine, so cross-backend divergence is logged for diagnostic value but never fails a run. Within-backend mismatches are surfaced as `"determinism_mismatch": true` on the cell and shown as a warning in the report, not silently averaged — they indicate a tool-specific sampler quirk or a harness bug.

**Positioning content**

- **R142.** **`docs/benchmarks/` directory** containing: `methodology.md` (the contract — what is and isn't being measured, how to read the numbers, per-tool fairness notes, prerequisites for re-running), `index.md` (chronological list of dated results pages), `results-<YYYY-MM-DD>.md` (the rendered tables + SVG charts produced by R138), `runs/<host-id>/*.json` (the raw data). Linked from `README.md` under a new "Benchmarks" section between "Features" and "Install."
- **R143.** **README section.** A new short section ("Benchmarks") with: one-paragraph framing, **one headline chart** (decode tok/s, mid-size model, normalized mode, across the four tools on whichever backend has the cleanest matrix coverage), and a single sentence pointing to `docs/benchmarks/results-<DATE>.md`. No long-form numbers in the README itself — it links out.
- **R144.** **Methodology doc is unconditional; blog post is conditional and gated separately.** `docs/benchmarks/methodology.md` and the first dated `results-<DATE>.md` ship as soon as the harness produces clean numbers on at least one backend, regardless of how favorable the numbers are. The launch-ready blog post at `docs/benchmarks/2026-XX-XX-vs-ollama-lm-studio.md` is drafted from those numbers but only shipped after a separate review pass that confirms there is a real surprise / win / honest finding worth a narrative — not as a marketing artifact for an unremarkable result. If the numbers come out tied within noise (the architecturally-likely outcome since all four tools run llama.cpp variants), the methodology + results page stand on their own and the blog post slot becomes a different artifact (e.g. "what the methodology revealed about defaults vs normalized" or "what the four tools each do uniquely well"). Tone in either case: confident about LlamaStash's architectural angle (zero overhead vs raw), honest about what end-to-end comparison can and can't tell you.

## Non-Goals

- **R145.** Model-quality comparison. The benchmark measures speed and resource cost, not eval scores. No HumanEval / MMLU / Aider runs in this slice — that's a separate question with a separate harness, and the speed comparison stands on its own.
- **R146.** GUI / UX comparison. LM Studio's GUI vs LlamaStash's TUI is a different conversation with different metrics (time to first chat for a new user, discoverability of advanced features, etc.). Worth doing eventually; not this brainstorm.
- **R147.** Native non-llama.cpp engines. LM Studio's MLX backend, vLLM, mlc-llm, exllamav2, and similar are out of v1 even when they could run the same model — apples-to-apples requires the same engine family, and LM Studio's MLX path is forced off in normalized mode. Future: a separate "MLX vs llama.cpp on Apple Silicon" comparison.
- **R148.** Cloud / hosted endpoints. OpenRouter, Replicate, vLLM-hosted, Anthropic, OpenAI etc. are out. Local-only.
- **R149.** Windows. v1 ships Linux + macOS only, matching LlamaStash's current platform support. Adding Windows is gated on LlamaStash itself supporting it.
- **R150.** "Try to make LlamaStash win." The harness must be willing to publish unflattering numbers if they're real. If end-to-end decode tok/s comes out worse than Ollama on some cell, the result ships; the surprise is the story.

## Outstanding Questions

- **Q1 — LM Studio normalization ceiling.** The `lms` CLI may not expose every flag we want to force (batch / ubatch sizes, KV cache type, flash_attn). Proposal: run with as-close-as-possible normalization, mark the cell `"unfair_knob": [...]`, document which knobs couldn't be set in `methodology.md`. **Decide during planning** after a dry run on one backend.
- **Q2 — Ollama Modelfile vs OpenAI API parameter precedence.** Ollama lets `Modelfile PARAMETER` and per-request OpenAI parameters both set `num_ctx`, `temperature`, etc. The OpenAI shim ignores some `Modelfile` settings and respects others; verifying this is part of v1 and the verification belongs in `methodology.md`.
- **Q3 — Two "large" cells vs one.** R128 now defines `large-dense` (14B-class) and `large-moe` (30B-A3B-class) as two named cells. The 14B keeps "large" runnable on more configs; the MoE is where Ollama / LM Studio historically diverge from upstream. Open question: does the harness run both on every config that can host either, or pick exactly one per host? **Defer to planning after a dry run** on at least one NVIDIA + Apple Metal box to see if the MoE cell adds enough new signal to justify its runtime.
- **Q4 — GPU backend version skew.** Each tool ships its own CUDA / Metal builds against its own pinned `llama.cpp` commit. Recording the exact `llama.cpp` commit per tool (when discoverable from the binary; Ollama embeds a version) would let readers attribute differences to engine vs wrapper. Aspirational for v1; if not achievable per-tool, at least record what's discoverable.
- **Q5 — ~~CI runner cost / cadence.~~ [Resolved 2026-05-21]** Decided: no CI for either suite. Maintainer-run on demand per R125. Reopen only if maintenance ownership transfers to a team with infra appetite.
- **Q6 — Tool warmup philosophy.** Should the cold-launch TTFT in Suite B include the time it takes Ollama / LM Studio to load the model on first request (their lazy-load path), or only the post-load TTFT? Both are valid; the first is what users feel, the second is engine-comparable. **Proposal: report both as separate metrics**, defer the final decision to planning after seeing the actual numbers.

## Success Criteria

- A maintainer introduces a synthetic 50 ms `sleep` in `src/ipc/methods.rs::start_model` and runs the overhead suite on their local box. The harness prints a clear "TTFT delta 67 ms — advisory tier exceeded (>30 ms)" warning (and would hard-fail at >200 ms). The maintainer notices before tagging the release.
- A user with an NVIDIA box runs `./scripts/bench/end-to-end/run.sh --backend nvidia --models small,mid` and produces a R134-shape JSON. The maintainer drops it into `docs/benchmarks/runs/<their-host-id>/` and re-renders; the new numbers appear in the next results page without code changes.
- The published `docs/benchmarks/results-<DATE>.md` lets a skeptical reader (a) read the exact tool versions used, (b) see variance not just means, (c) tell which cells are "defaults" vs "normalized," (d) follow a link to the script that produced it and run it themselves.
- The launch post links to the methodology doc inside its first three paragraphs; the chart in the post can be re-derived from the published raw JSON without contacting the author.
- The normalized-mode fairness self-check (R141) catches any future divergence in greedy decoding across tools. If a tool's sampler quietly stops being deterministic, the harness surfaces it loudly.
- If LlamaStash's end-to-end numbers turn out worse than a competitor on a cell, the result ships truthfully and a follow-up TODO captures the investigation. The benchmark is a measurement tool, not a marketing tool.
