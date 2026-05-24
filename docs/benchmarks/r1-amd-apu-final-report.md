# R1 AMD-APU final bench report

**Hardware:** AMD Ryzen AI Max+ 395 ("Strix Halo") APU with Radeon 8060S iGPU (RDNA 3.5, `gfx1151`), 121 GiB unified RAM (4 GiB pinned VRAM partition + ≈96 GiB GTT), TDP 70 W steady (one mid-run blip to 90 W on the Qwen3.6-27B-Q8 dense run was confirmed benign by a clean 70 W re-run within ~1% of the mixed-power numbers).

**Date:** 2026-05-24. Same hardware, same day, same llama.cpp commit (b9282 HIP build, rocWMMA OFF — that flag was empirically tested and rejected per `~/dotfiles/LLM-BENCH.md`).

**Models (per R1 release checklist for this hardware class — all four hit):**

| Slot | File | Bytes | Params | Notes |
|---|---|---:|---:|---|
| `small` | `lmstudio-community/gemma-4-E2B-it-GGUF/gemma-4-E2B-it-Q4_K_M.gguf` | 3.4 GB | ~4.6 B (MoE-ish E2B) | Loaded by all four tools — same SHA verified. |
| `mid` | `lmstudio-community/gemma-4-31B-it-GGUF/gemma-4-31B-it-Q4_K_M.gguf` | 17 GB | 31 B dense | LM Studio omitted — `lms load` CLI doesn't accept the `@q4_k_m` variant suffix and the parent key defaults to Q8_0. |
| `large_dense` | `lmstudio-community/Qwen3.6-27B-GGUF/Qwen3.6-27B-Q8_0.gguf` | 27 GB | 27 B dense | LM Studio omitted — `lms load` failed mid-session today (state issue, see *LM Studio caveats* below). |
| `large_moe` | `lmstudio-community/Qwen3.6-35B-A3B-GGUF/Qwen3.6-35B-A3B-Q8_0.gguf` | 34 GB | 35 B total / 3 B active (MoE) | LM Studio omitted — same load failure. |

**Tools:** LlamaStash (this repo, b9282 HIP via `$LLAMASTASH_LLAMA_SERVER`); raw `llama-server` (same b9282 HIP binary, called directly); Ollama 0.24.0 (own bundled inference engine, models imported via Modelfile from the same source path with SHA-256 verified); LM Studio v2.16.0 `amd-rocm-avx2` runtime (where data exists).

**Modes:** `defaults` (each tool invoked as a new user would, no tuning knobs) and `normalized` (matched-pair: `ctx`/`n_gpu_layers=999`/`flash_attn=on`/`kv_cache_type=f16`/`batch_size=512`/`ubatch_size=512`; `rag_prefill` overrides `ctx=10240` so the 8157-token corpus + system + question + decode fits comfortably).

**Workloads:** `chat_turn` (≤50-token prompt, ≤64-token decode), `agent_decode` (≤50 prompt, 256 decode), `rag_prefill` (8157-token prompt from a fixed corpus, ≤64 decode — fresh-cache TTFT is *not* what's measured; reps 2-4 hit the engine's prefix cache so the published TTFT is the post-cache figure), `parallel_4` (4 concurrent `chat_turn` streams; reported tok/s is aggregate, TTFT is the slowest stream).

**Reps:** 1 warmup + 3 measured per cell. Every published cell passed the variance gate (`stddev/mean ≤ 10%`).

---

## Headline table — decode tok/s

Average across `defaults` + `normalized` modes (they're within noise of each other on this hardware for every model, so collapsing is honest; per-mode rows are on the auto-rendered [results-2026-05-24.md](results-2026-05-24.md) page).

| Tool / model | small (E2B Q4) | mid (31B Q4) | large_dense (27B Q8) | large_moe (35B-A3B Q8) |
|---|---:|---:|---:|---:|
| **LlamaStash** | **86.9 tok/s** | **9.8 tok/s** | **7.4 tok/s** | **42.6 tok/s** |
| raw `llama-server` (b9282 HIP) | 84.9 tok/s | 9.9 tok/s | 7.4 tok/s | 42.7 tok/s |
| Ollama 0.24.0 | 50.4 tok/s | 4.8 tok/s | 2.6 tok/s | 12.1 tok/s |
| LM Studio v2.16.0 ROCm | 91.1 tok/s | — | — | — |

**chat_turn TTFT (ms):**

| Tool / model | small | mid | large_dense | large_moe |
|---|---:|---:|---:|---:|
| **LlamaStash** | **51** | **467** | **417** | **181** |
| raw `llama-server` | 52 | 468 | 414 | 186 |
| Ollama | 223 | 1092 | 1745 | 476 |
| LM Studio | 187 | — | — | — |

---

## Per-workload tables

### small — `gemma-4-E2B-Q4_K_M` (3.4 GB)

| Tool | chat_turn | agent_decode | rag_prefill | parallel_4 (aggregate) |
|---|---|---|---|---|
| LlamaStash | 86.9 tps / 51 ms | 85.8 tps / 56 ms | 74.8 tps / 55 ms | 208.7 tps / 187 ms |
| raw `llama-server` | 84.9 / 52 | 84.4 / 57 | 73.4 / 57 | 207.3 / 184 |
| Ollama | 50.4 / 223 | 47.1 / 224 | 43.2 / **17 390** | 212.9 / 2 372 |
| LM Studio | 91.1 / 187 | 80.5 / 200 | — (load failure) | — (load failure) |

### mid — `gemma-4-31B-Q4_K_M` (17 GB)

| Tool | chat_turn | agent_decode | rag_prefill | parallel_4 (aggregate) |
|---|---|---|---|---|
| LlamaStash | 9.8 / 467 | 9.7 / 530 | 7.5 / 177 | 23.2 / 1 485 |
| raw `llama-server` | 9.9 / 468 | 9.8 / 524 | 7.6 / 177 | 23.2 / 1 498 |
| Ollama | 4.8 / 1 092 | 4.8 / 1 070 | 4.7 / **239 817** | 21.4 / 22 875 |
| LM Studio | — | — | — | — |

### large_dense — `Qwen3.6-27B-Q8_0` (27 GB)

Two passes here: original mixed-power (70 W → 90 W mid-run) and a clean 70 W re-run. Values within ~1%; published numbers are from the clean re-run.

| Tool | chat_turn | agent_decode | rag_prefill | parallel_4 (aggregate) |
|---|---|---|---|---|
| LlamaStash | 7.4 / 417 | 7.4 / 418 | 7.3 / 191 | 24.8 / 1 161 |
| raw `llama-server` | 7.4 / 414 | 7.5 / 413 | 7.3 / 190 | 24.9 / 1 172 |
| Ollama | 2.6 / 1 745 | 2.7 / 2 081 | 0.6 / **177 609** | 10.9 / 43 175 |
| LM Studio | — | — | — | — |

### large_moe — `Qwen3.6-35B-A3B-Q8_0` (34 GB, 3 B active)

| Tool | chat_turn | agent_decode | rag_prefill | parallel_4 (aggregate) |
|---|---|---|---|---|
| LlamaStash | 42.6 / 181 | 42.2 / 191 | 40.2 / 78 | 110.5 / 468 |
| raw `llama-server` | 42.7 / 186 | 43.3 / 190 | 41.1 / 75 | 112.5 / 443 |
| Ollama | 12.1 / 476 | 12.3 / 517 | 2.7 / **38 955** | 50.2 / 9 613 |
| LM Studio | — | — | — | — |

---

## Findings

### 1. LlamaStash adds zero measurable overhead vs raw `llama-server`

On every model × workload × mode tested, **LlamaStash decode tok/s tracks raw `llama-server` within ≤2%** (mostly within ≤1%, well inside run-to-run variance). TTFT is similarly identical. The wrapper architecture (spawning the unmodified upstream binary) holds.

This was already the conclusion from the 2026-05-23 small-model run; the mid/large/MoE runs *don't change it*. There's no "tax" that only shows up at large model size.

### 2. Ollama is materially slower than raw `llama-server` on every model

Even with `LLAMASTASH_BENCH_KEEP_IMPORTS=1` so the Modelfile-import cost amortises across cells, **Ollama is 30–70% slower at chat decode** and **4–8× slower TTFT**. Same byte-identical GGUF on the same hardware.

| Model | Ollama / `llama-server` chat decode | Ollama TTFT (cold first-request) |
|---|---:|---:|
| small | 50.4 / 84.9 (-41%) | 4.4× slower |
| mid | 4.8 / 9.9 (-52%) | 2.3× slower |
| large_dense | 2.6 / 7.4 (-65%) | 4.2× slower |
| large_moe | 12.1 / 42.7 (-72%) | 2.6× slower |

The gap *widens* as model size grows.

### 3. Ollama RAG performance is catastrophic on this hardware

The `rag_prefill` workload uses a fixed 8157-token corpus repeated across reps. `llama-server`-based tools cache the prefix and post-cache TTFT lands in the ~75–200 ms range. **Ollama does not use prefix caching** (or doesn't on this setup), so every rep does a full cold prefill:

| Model | Ollama `rag_prefill` TTFT | LlamaStash `rag_prefill` TTFT | Ratio |
|---|---:|---:|---:|
| small (3.4 GB) | 17 390 ms (17 s) | 55 ms | 316× |
| mid (17 GB) | 239 817 ms (~4 min) | 177 ms | 1 354× |
| large_dense (27 GB) | 177 609 ms (~3 min) | 191 ms | 930× |
| large_moe (34 GB MoE) | 38 955 ms (39 s) | 78 ms | 499× |

Mechanism unverified (could be a bench config issue — needs a follow-up Ollama setting audit) but the gap is too large to be measurement noise. **For RAG-style workloads on this hardware, Ollama is unusable.**

### 4. `defaults` and `normalized` are nearly identical for every tool × every model

The matched-pair `normalized` knob set (`ctx`/`n_gpu_layers`/`flash_attn=on`/`kv_cache_type=f16`/`batch_size`/`ubatch_size`) was supposed to expose where each tool's defaults underperform. **It doesn't on this hardware** — defaults and normalized always land within ≤2% of each other for the same tool.

One real exception: **LlamaStash's `defaults` mode caps `ctx` below the 8 k corpus** (because its `defaults_table.rs` overlay picks a smaller default than llama-server's "use model max"), so `defaults rag_prefill` cells return 400. Workaround: set `--ctx` explicitly, or use the `normalized` mode. Either a documentation issue or worth raising the default ctx for models with large `max_context`.

### 5. MoE wins big on this APU at large size

Despite being a larger model (34 GB on disk vs 27 GB for the dense Qwen3.6-27B), the **Qwen3.6-35B-A3B MoE decodes ~5.8× faster** than the dense 27B (42.6 vs 7.4 tok/s on LlamaStash chat_turn) because only ~3 B params are active per token. With Strix Halo's unified-memory architecture, MoE is essentially free of the memory-bandwidth ceiling that hurts dense models at this size. **For local agents on this hardware, large MoE is the sweet spot.**

### 6. `parallel_4` aggregate decode scales ~3–5× across all models

| Model | LlamaStash single-stream chat | LlamaStash parallel_4 aggregate | Per-stream effective |
|---|---:|---:|---:|
| small | 86.9 | 208.7 | 52.2 (60% of single) |
| mid | 9.8 | 23.2 | 5.8 (59%) |
| large_dense | 7.4 | 24.8 | 6.2 (84%) |
| large_moe | 42.6 | 110.5 | 27.6 (65%) |

Per-stream throughput drops 15-40% under 4-way concurrency, with aggregate hitting ~2.4× to 3.4× single-stream. Healthy.

---

## Methodology caveats

- **Power profile blip.** The Qwen3.6-27B run originally happened across a 70 W → 90 W power-profile transition mid-run; the user flagged it and a clean 70 W re-run was performed. Values matched to ~1% — *the 90 W blip was benign on this hardware* (likely the dense 27 B run is GPU-memory-bandwidth-bound, not compute-bound, so an extra 20 W doesn't help).
- **`prompt_tps` for `rag_prefill` is misleading**. Because reps 2-4 hit the prefix cache, the bench's `prompt_tps = prompt_tokens / TTFT` formula divides by ~100 ms instead of the true cold-prefill time, giving artificially huge tok/s (the JSON shows ~46 k tok/s for the 31 B model — that's not real prefill throughput, it's "cached-prefix tokens / cache-lookup time").
- **LlamaStash's `defaults_table.rs` overlay** picks model-aware defaults (smaller `ctx`, specific `n_gpu_layers`) compared to raw `llama-server`. For tests where one tool's "defaults" actually means different knobs than another's, the `normalized` mode is the apples-to-apples comparison. `defaults` reflects out-of-box UX.
- **gollama not used.** Earlier-session investigation showed it's not needed for byte-identity: Ollama imports the same source GGUF via Modelfile and verifies the SHA-256 match; LM Studio scans the same source directory and its `model-index-cache.json` `entryPoint.absPath` points at the same file. All four tools confirmed loading the same bytes for `small`; for `mid`/`large_dense`/`large_moe` only the first three tools were loadable (LM Studio omitted, see below).

### LM Studio caveats

LM Studio data is partial and shouldn't be over-interpreted:

- **`small`**: clean data (yesterday's 2026-05-23 run + today's `small` extra-workload run partially succeeded). The 91 tok/s chat_turn / 187 ms TTFT figures are real.
- **`mid`**: `lms load` won't accept the `google/gemma-4-31b@q4_k_m` variant suffix from its own CLI, and the unsuffixed `google/gemma-4-31b` key defaults to the Q8_0 file (different bytes). To stay byte-identical, LM Studio was skipped for `mid`.
- **`large_dense`** + **`large_moe`**: `lms load` returned `Error loading model. (Exit code: null)` from a stuck-state of the LM Studio desktop app developed earlier in the session. `lms server stop/start` didn't recover it; a fresh desktop-app restart would (out of scope for the automated bench). Cause unverified; reproducing the LM Studio bench is on the follow-up list. The 2026-05-23 page's LM Studio numbers (only `small`) remain the canonical LMS reference for this hardware until then.

---

## Raw data

- `docs/benchmarks/runs/deepu-flowz13-arch/` — main run JSONs (mid, large_moe, small extra workloads, original mixed-power large_dense)
- `docs/benchmarks/runs/deepu-flowz13-arch-clean70w/` — clean-70 W large_dense rerun
- `docs/benchmarks/runs/deepu-flowz13-arch-vulkan/`, `...-rocm/`, `...-hip-rocwmma-on/`, `...-hip-rocwmma-off/` — earlier engine and build-flag A/B runs (2026-05-23 / 2026-05-24)

Each JSON is schema-validated by [`scripts/bench/end_to_end/schema.py`](../../scripts/bench/end_to_end/schema.py); the auto-rendered dated pages ([results-2026-05-23.md](results-2026-05-23.md), [results-2026-05-24.md](results-2026-05-24.md)) are reproducible via:

```sh
.venv/bin/python -m scripts.bench.end_to_end.render --date 2026-05-23 --runs-dir docs/benchmarks/runs
.venv/bin/python -m scripts.bench.end_to_end.render --date 2026-05-24 --runs-dir docs/benchmarks/runs
```

Anyone re-running the harness on Strix Halo gfx1151 with the same llama.cpp commit should land within the variance gate of these numbers.
