# R1 AMD-APU final bench report

> **2026-06-01 update:** numbers in this report were the original 2026-05-24 run before the engine-mixing audit. The headline cross-tool cells for LlamaStash and raw `llama-server` (e.g. `small chat_turn 86.9 / 51`) average HIP and Vulkan engine paths into a single cell. The audited engine-clean tables are in [`docs/benchmarks.md`](../benchmarks.md#amd-apu---linux) and in the public post — they use single-source single-engine cells (HIP only for the main table, Vulkan only for the addendum). This report stays as the historical 2026-05-24 record, with the engine-A/B finding (#2 below) and the raw JSONs unchanged.

**Hardware:** AMD Ryzen AI Max+ 395 ("Strix Halo") APU with Radeon 8060S iGPU (RDNA 3.5, `gfx1151`), 121 GiB unified RAM (4 GiB pinned VRAM partition + ≈96 GiB GTT), TDP 70 W steady (one mid-run blip to 90 W on the Qwen3.6-27B-Q8 dense run was confirmed benign by a clean 70 W re-run within ~1% of the mixed-power numbers).

**Date:** 2026-05-24. Same hardware, same day, same local llama.cpp build recorded in the benchmark JSON provenance as `version: 9245 (b39a7bf1b)` for the LlamaStash + raw `llama-server` cells (HIP build, `GGML_HIP_ROCWMMA_FATTN=OFF` per the earlier empirical finding documented in `~/dotfiles/LLM-BENCH.md`).

**Models (per R1 release checklist for this hardware class — all four covered):**

| Slot | File | Bytes | Params | Notes |
|---|---|---:|---:|---|
| `small` | `lmstudio-community/gemma-4-E2B-it-GGUF/gemma-4-E2B-it-Q4_K_M.gguf` | 3.4 GB | ~4.6 B (E2B) | All four tools verified loading the same SHA. |
| `mid` | `lmstudio-community/gemma-4-31B-it-GGUF/gemma-4-31B-it-Q4_K_M.gguf` | 17 GB | 31 B dense | LM Studio loaded the same Q4_K_M file via the `google/gemma-4-31b@q4_k_m` modelKey through its OpenAI-compat shim (the `lms load` CLI rejects the suffix, but the shim auto-loads on first request — driver updated to use that). |
| `large_dense` | `lmstudio-community/Qwen3.6-27B-GGUF/Qwen3.6-27B-Q8_0.gguf` | 27 GB | 27 B dense | All four. LMS cell ran on its **Vulkan** runtime (its ROCm runtime entered a stuck-state mid-session that survives both `lms server stop/start` and a desktop-app restart — likely AMD ROCm driver issue needing reboot). |
| `large_moe` | `lmstudio-community/Qwen3.6-35B-A3B-GGUF/Qwen3.6-35B-A3B-Q8_0.gguf` | 34 GB | 35 B total / 3 B active | All four. LMS on Vulkan as above. |

**Tools:**

- **LlamaStash** (this repo) — `LLAMASTASH_LLAMA_SERVER` pointed at the local HIP build recorded as `version: 9245 (b39a7bf1b)` for the main numbers; a Vulkan build of the same version was used for the engine A/B on `small` (full matrix) and `large_dense` (full matrix).
- **raw `llama-server`** — same local 9245 HIP / Vulkan binaries, invoked directly.
- **Ollama 0.24.0** — own bundled engine. Each test GGUF imported on demand via Modelfile (SHA-256 verified against source).
- **LM Studio** desktop with bundled `llama.cpp-linux-x86_64-vulkan-avx2-2.16.0` runtime. We *attempted* the bundled `amd-rocm-avx2 v2.16.0` runtime for mid / large_dense / large_moe both during the main run and again after a full system reboot. Both attempts failed identically (`Error loading model. Exit code: null`, inference child process crashes within ~5 s of any load). Treated as a known LMS / gfx1151 bundle incompatibility; LMS-Vulkan stays as the reference for mid / large_dense / large_moe. Small-model LMS data is from yesterday's clean LMS-ROCm session, and the engine-A/B on small showed LMS-ROCm ≈ LMS-Vulkan within ~1% on this hardware.

**Modes:** `defaults` and `normalized` (matched-pair `ctx`/`n_gpu_layers=999`/`flash_attn=on`/`kv_cache_type=f16`/`batch_size=512`/`ubatch_size=512`; `rag_prefill` overrides `ctx=10240` so the 8157-token corpus + system + question + decode all fit).

**Workloads:** `chat_turn`, `agent_decode`, `rag_prefill`, `parallel_4`.

**Reps:** 1 warmup + 3 measured per cell. Every published cell (except where noted) passed the variance gate (`stddev/mean ≤ 10%`).

---

## Headline table — decode tok/s

Average across `defaults` + `normalized` modes (within ~1% on this hardware for every tool/model, so collapsing is honest).

| Tool / model | small (E2B Q4) | mid (31B Q4) | large_dense (27B Q8) | large_moe (35B-A3B Q8) |
|---|---:|---:|---:|---:|
| **LlamaStash** | **86.9 tok/s** | 9.8 tok/s | **7.4 tok/s** | **42.6 tok/s** |
| raw `llama-server` (local build) | 86.0 tok/s | 9.9 tok/s | 7.4 tok/s | 42.7 tok/s |
| LM Studio (v2.16.0; small=ROCm, mid/large=Vulkan) | **91.1 tok/s** | **11.6 tok/s** | **7.9 tok/s** | 37.0 tok/s |
| Ollama 0.24.0 | 50.4 tok/s | 4.8 tok/s | 2.6 tok/s | 12.1 tok/s |

**chat_turn TTFT (ms):**

| Tool / model | small | mid | large_dense | large_moe |
|---|---:|---:|---:|---:|
| **LlamaStash** | **51** | **467** | **417** | **181** |
| raw `llama-server` | 51 | 468 | 414 | 186 |
| LM Studio | 187 | 1 477 | 1 274 | 683 |
| Ollama | 223 | 1 092 | 1 745 | 476 |

---

## Per-workload tables (decode tok/s / TTFT ms)

### small — `gemma-4-E2B-Q4_K_M` (3.4 GB)

Raw `llama-server` small-model summary cells use the standard HIP + Vulkan rows only. The separate rocWMMA on/off side experiment is reported later and is not folded into these headline averages.

| Tool | chat_turn | agent_decode | rag_prefill | parallel_4 (aggregate) |
|---|---|---|---|---|
| LlamaStash | 86.9 / 51 | 85.8 / 56 | 74.8 / 55 | 208.7 / 187 |
| raw `llama-server` | 86.0 / 51 | 85.7 / 56 | 73.4 / 57 | 207.3 / 184 |
| LM Studio | 91.1 / 187 | 80.5 / 200 | — (load failure) | — (load failure) |
| Ollama | 50.4 / 223 | 47.1 / 224 | 43.2 / **17 390** | 212.9 / 2 372 |

### mid — `gemma-4-31B-Q4_K_M` (17 GB)

| Tool | chat_turn | agent_decode | rag_prefill | parallel_4 (aggregate) |
|---|---|---|---|---|
| LlamaStash | 9.8 / 467 | 9.7 / 530 | 7.5 / 177 | 23.2 / 1 485 |
| raw `llama-server` | 9.9 / 468 | 9.8 / 524 | 7.6 / 177 | 23.2 / 1 498 |
| LM Studio | **11.6 / 1 477** | **10.2 / 1 615** | **10.2 / 1 285** | **37.1 / 3 730** |
| Ollama | 4.8 / 1 092 | 4.8 / 1 070 | 4.7 / **239 817** | 21.4 / 22 875 |

### large_dense — `Qwen3.6-27B-Q8_0` (27 GB)

Full 2×2 engine A/B for LlamaStash + raw `llama-server` (HIP and Vulkan, same local 9245 build, two compile targets). Earlier mixed-power (70 W → 90 W mid-run) was confirmed benign by a clean 70 W re-run within ~1%; published HIP numbers are from the clean re-run.

| Tool | Engine | chat_turn | agent_decode | rag_prefill | parallel_4 (aggregate) |
|---|---|---|---|---|---|
| LlamaStash | **HIP** | 7.4 / **417** | 7.4 / **418** | 7.3 / 191 | 24.8 / **1 161** |
| LlamaStash | **Vulkan** | 7.4 / 743 | 7.4 / 786 | 7.3 / 204 | 25.2 / 2 226 |
| raw `llama-server` | HIP | 7.4 / 414 | 7.5 / 413 | 7.3 / 190 | 24.9 / 1 172 |
| raw `llama-server` | Vulkan | 7.5 / 720 | 7.5 / 770 | 7.3 / 198 | 25.2 / 2 189 |
| LM Studio | Vulkan | **7.9 / 1 274** | **7.5 / 1 466** | **TTFT only: 84 ms** (decode null — see caveat) | **22.6 / 2 886** |
| Ollama | bundled | 2.6 / 1 745 | 2.7 / 2 081 | 0.6 / **177 609** | 10.9 / 43 175 |

Engine takeaway for this model: **decode is engine-independent within 1%** (memory-bandwidth bound), but **HIP is ~75–90% faster on TTFT** for short-prompt workloads and **2× faster on parallel_4 TTFT**. Vulkan only matches HIP TTFT on `rag_prefill` where post-cache TTFT is dominated by the cache lookup. See Finding #2 for full discussion.

### large_moe — `Qwen3.6-35B-A3B-Q8_0` (34 GB on disk, 3 B active per token)

| Tool | chat_turn | agent_decode | rag_prefill | parallel_4 (aggregate) |
|---|---|---|---|---|
| LlamaStash | 42.6 / 181 | 42.2 / 191 | 40.2 / 78 | 110.5 / 468 |
| raw `llama-server` | 42.7 / 186 | 43.3 / 190 | 41.1 / 75 | 112.5 / 443 |
| LM Studio | 37.0 / 683 | 35.7 / 718 | **TTFT only: 75 ms** (decode null — see caveat) | 95.7 / 1 203 |
| Ollama | 12.1 / 476 | 12.3 / 517 | 2.7 / **38 955** | 50.2 / 9 613 |

---

## Findings

### 1. LlamaStash adds zero measurable overhead vs raw `llama-server`

On every model × workload × mode tested, **LlamaStash decode tok/s tracks raw `llama-server` within ≤2%** (mostly within ≤1%, well inside run-to-run variance). TTFT is similarly identical. The wrapper architecture (spawning the unmodified upstream binary) holds across the full R1 size range.

### 2. Engine choice (HIP vs Vulkan) is workload- and model-size-dependent — not a simple "Vulkan wins"

The small-model engine A/B (run earlier today) showed our local **Vulkan build of 9245 ~17–20% faster than HIP** on `chat_turn` / `agent_decode`. That single data point was misleading. Re-running the same A/B properly on `large_dense` (Qwen3.6-27B-Q8) — same local 9245 build, two compile targets, run on a clean GPU after a power-state reset — paints a very different picture:

| Workload | LlamaStash HIP | LlamaStash Vulkan | Δ |
|---|---:|---:|---:|
| chat_turn | 7.3 tok/s @ **421 ms** | 7.4 tok/s @ **743 ms** | decode ≈, TTFT **+76%** |
| agent_decode | 7.4 @ 414 | 7.4 @ 786 | decode ≈, TTFT +90% |
| rag_prefill | 7.3 @ 192 | 7.3 @ 204 | both ≈ |
| parallel_4 | 24.6 @ 1 174 | 25.2 @ 2 226 | decode ≈, TTFT +90% |

So for **large dense Q8 on this hardware:**

- **Decode throughput is essentially identical** between HIP and Vulkan (within 1%).
- **TTFT is dramatically worse on Vulkan** for short-prompt workloads (chat_turn, agent_decode, parallel_4 each see ~75–90% higher TTFT). The fixed setup cost on Vulkan looks higher per-request, possibly from pipeline-state / descriptor-set initialisation. On `rag_prefill` (which post-cache hit reduces TTFT to a near-no-op), the engines tie.

**Why the small model showed Vulkan winning and the large dense Q8 didn't:** on small, decode tok/s is ~80, so even modest per-token wins translate to large delta absolutes. On large_dense Q8, decode is memory-bandwidth-bound at ~7 tok/s (we're saturating the unified-memory bandwidth ceiling, not GPU compute), so engine differences in compute kernels don't move the needle. The Vulkan TTFT regression *does* show up because that's purely setup-cost dominated. The [llama.cpp Issue #13565](https://github.com/ggml-org/llama.cpp/issues/13565) cited earlier is a real upstream issue but the empirical answer is much more nuanced than "Vulkan beats HIP."

**LM Studio's bundled `amd-rocm-avx2 v2.16.0` runtime is broken on gfx1151** — it crashes the inference child process within ~5 seconds of any `lms load`, both via the CLI and via the OpenAI shim's auto-load. The failure persists across `lms server stop/start`, a full LM Studio desktop-app restart, **and a full system reboot**. Conclusion: it's a real LMS/ROCm bundle bug for this hardware, not a transient stuck state. LMS users on Strix Halo should use the Vulkan runtime. The LMS mid/large_dense/large_moe numbers in this report are all from LMS's `vulkan-avx2 v2.16.0` runtime; the small-model LMS-ROCm vs LMS-Vulkan A/B from earlier today showed ≤1% delta between the two on that hardware, so LMS-Vulkan is a reasonable proxy for what LMS-ROCm would produce *if* the bundled ROCm runtime weren't crashing.

LMS pays a consistent **~1–1.5 s TTFT tax** vs direct `llama-server` regardless of engine, due to the OpenAI shim + LMS's reasoning-mode parser overhead.

### 3. Ollama is materially slower than the other three tools on every model

Even with `LLAMASTASH_BENCH_KEEP_IMPORTS=1`:

| Model | Ollama / raw llama-server chat decode | Ollama TTFT vs raw |
|---|---:|---:|
| small | 50.4 / 86.0 (−41%) | 4.3× slower |
| mid | 4.8 / 9.9 (−52%) | 2.3× slower |
| large_dense | 2.6 / 7.4 (−65%) | 4.2× slower |
| large_moe | 12.1 / 42.7 (−72%) | 2.6× slower |

The gap *widens* as model size grows. Mechanism unverified — could be Ollama's bundled llama.cpp build flags, kernel selection, or GPU offload heuristics on this specific APU; would need a per-tool runtime-spec deep-dive to confirm.

### 4. Ollama RAG performance is catastrophic on this hardware

The `rag_prefill` workload uses a fixed 8157-token corpus repeated across reps. `llama-server`-based tools (LlamaStash, raw, LMS) cache the prefix and post-cache TTFT lands in the 75–1 300 ms range. **Ollama does not use prefix caching here** — every rep does a full cold prefill:

| Model | Ollama `rag_prefill` TTFT | Best other tool TTFT | Ratio |
|---|---:|---:|---:|
| small (3.4 GB) | 17 390 ms (17 s) | 55 ms (llamastash) | 316× |
| mid (17 GB) | 239 817 ms (~4 min) | 177 ms | 1 354× |
| large_dense (27 GB) | 177 609 ms (~3 min) | 84 ms (LMS shim) | 2 114× |
| large_moe (34 GB MoE) | 38 955 ms (~39 s) | 75 ms | 519× |

The mechanism is "no prefix cache on the bench's repeated-corpus workload" but the specific Ollama config knob that fixes this is unverified; **for RAG-style workloads on this hardware in default configuration, Ollama is unusable** without a follow-up tuning audit.

### 5. `defaults` and `normalized` are nearly identical across every tool × every model

The matched-pair `normalized` knob set was supposed to expose where each tool's defaults underperform. **It doesn't on this hardware** — defaults and normalized always land within ≤2% of each other for the same tool. The only meaningful exception:

- **LlamaStash's `defaults` mode caps `ctx` below the 8 k corpus** (because its `defaults_table.rs` overlay picks a smaller default than llama-server's "use model max"), so `defaults rag_prefill` returns HTTP 400 on llamastash specifically. `normalized` mode works (sets `ctx=10240` explicitly). Either a documentation issue or worth raising the default-table ctx for models with high `max_context`.

### 6. MoE wins big at large size on this APU

Despite being larger on disk (34 GB vs 27 GB), **Qwen3.6-35B-A3B MoE decodes ~5.8× faster** than the dense Qwen3.6-27B-Q8 (42.6 vs 7.4 tok/s on LlamaStash chat_turn). On Strix Halo's unified-memory architecture with 96 GB GTT available, MoE escapes the memory-bandwidth ceiling that hurts dense models. **For local agents on this hardware, large MoE is the sweet spot** — bigger total parameter count, faster effective decode.

### 7. `parallel_4` aggregate decode scales 2.4–3.4× across all models

| Model | LlamaStash single-stream chat | LlamaStash parallel_4 aggregate | Per-stream effective | vs single |
|---|---:|---:|---:|---:|
| small | 86.9 | 208.7 | 52.2 | 60% |
| mid | 9.8 | 23.2 | 5.8 | 59% |
| large_dense | 7.4 | 24.8 | 6.2 | 84% |
| large_moe | 42.6 | 110.5 | 27.6 | 65% |

Per-stream throughput drops 15–40% under 4-way concurrency, with aggregate hitting ~2.4× to 3.4× single-stream. Healthy. (large_dense's 84% per-stream is unusually high — probably because the dense 27B is memory-bandwidth-bound at single-stream too, so adding streams doesn't worsen it much.)

---

## Methodology caveats

- **Power profile blip.** The Qwen3.6-27B run originally happened across a 70 W → 90 W power-profile transition mid-run; the user flagged it and a clean 70 W re-run was performed. Values matched to ~1%, so the 90 W blip was benign — the dense 27 B run is GPU-memory-bandwidth-bound, not compute-bound, so an extra 20 W doesn't help.

- **`prompt_tps` for `rag_prefill` in the JSONs is inflated nonsense**. Because reps 2-4 hit the prefix cache, the formula `prompt_tps = prompt_tokens / TTFT` divides by ~100 ms instead of the true cold-prefill time, giving artificially huge tok/s. **Use `decode_tps` + `TTFT` as the real signals**, not `prompt_tps`, for rag_prefill.

- **LMS rag_prefill on large_dense and large_moe returned `decode_tps=null`** despite passing TTFT measurement. Cause: LM Studio's reasoning-mode parser splits `usage.completion_tokens` into `content` + `reasoning_tokens`; on these models the model emitted ≤1 content token before hitting `max_tokens=64` (with most tokens classified as reasoning), and the bench's `decode_tps` formula bails when `decode_tokens ≤ 1`. The TTFT values are valid (~75–90 ms — confirming the engine prefix-cached the corpus correctly).

- **LlamaStash's `defaults_table.rs` overlay** picks model-aware defaults (smaller `ctx`, specific `n_gpu_layers`) compared to raw `llama-server`. For tests where one tool's "defaults" actually means different knobs than another's, the `normalized` mode is the apples-to-apples comparison. `defaults` reflects out-of-box UX.

- **gollama not used.** Earlier-session investigation showed it's not needed for byte-identity: Ollama imports the same source GGUF via Modelfile and verifies the SHA-256 match; LM Studio scans the same source directory and the `model-index-cache.json entryPoint.absPath` field confirms each modelKey's actual file path. All four tools confirmed loading the same bytes for every model in this report.

### LM Studio engine note

- **`small` LMS data is from the ROCm runtime** (2026-05-23 run + 2026-05-24 engine A/B), with a matched Vulkan A/B row showing the two engines land within ~1% of each other on small.
- **`mid` / `large_dense` / `large_moe` LMS data is from the Vulkan runtime.** Two separate LMS-ROCm attempts (mid-session and again post-reboot) failed identically: `Error loading model. Exit code: null`, with the bundled inference child process exiting within ~5 s of any load — same outcome via the `lms load` CLI and the OpenAI shim's auto-load. The failure pattern (consistent, repeatable, survives system reboot) points at the bundled `amd-rocm-avx2 v2.16.0` runtime being incompatible with `gfx1151` on the current LM Studio desktop build — not a transient stuck state we mistook for one. LMS users on Strix Halo should select the Vulkan runtime; that's what these published numbers use.
- Engine A/B on `large_dense` (separate run, full 2×2 with LlamaStash + raw `llama-server`) confirms decode is engine-independent at this size (within 1% across HIP and Vulkan) while TTFT is workload-shape sensitive. The LMS-Vulkan numbers should track LMS-ROCm if it worked.

### LM Studio CLI vs shim discovery

The original LMS bench driver used `lms load <modelKey>` which can't pin a specific quant variant (`@q4_k_m` etc. are rejected). The OpenAI-compat shim *can* — sending a chat completion with `model: "google/gemma-4-31b@q4_k_m"` auto-loads the Q4 variant. The driver was rewritten this session to bypass `lms load` entirely and rely on the shim's auto-load, plus a preflight chat to trigger the load before the warmup rep. This is more robust (no CLI failures) and supports variant pinning.

---

## Raw data

- `docs/benchmarks/runs/deepu-flowz13-arch/` — main run JSONs (mid Q4, large_moe, small extra-workloads, large_dense original mixed-power, plus this morning's small-model + engine-A/B runs)
- `docs/benchmarks/runs/deepu-flowz13-arch-clean70w/` — clean 70 W large_dense rerun
- `docs/benchmarks/runs/deepu-flowz13-arch-lms-vulkan/` — LMS-only mid + large_dense + large_moe on Vulkan runtime
- `docs/benchmarks/runs/deepu-flowz13-arch-vulkan/`, `...-rocm/`, `...-hip-rocwmma-on/`, `...-hip-rocwmma-off/` — earlier engine and build-flag A/B runs

Each JSON is schema-validated by [`scripts/bench/end_to_end/schema.py`](../../scripts/bench/end_to_end/schema.py); the auto-rendered dated pages ([results-2026-05-23.md](results-2026-05-23.md), [results-2026-05-24.md](results-2026-05-24.md)) are reproducible from these JSONs via:

```sh
.venv/bin/python -m scripts.bench.end_to_end.render --date 2026-05-23 --runs-dir docs/benchmarks/runs
.venv/bin/python -m scripts.bench.end_to_end.render --date 2026-05-24 --runs-dir docs/benchmarks/runs
```

Anyone re-running the harness on Strix Halo gfx1151 with the same llama.cpp commit should land within the variance gate of these numbers.

The summary table at the top of this report can be regenerated from the raw JSONs anytime via:

```sh
make bench-table                              # all hosts
make bench-table -- --host deepu-flowz13-arch # this machine only
make bench-table -- --json > pivot.json       # machine-readable pivot
```

The `bench-table` tool auto-detects engine variants from `host_id` suffixes (`-vulkan`, `-rocm`, `-hip-rocwmma-on`, etc.) and pivots into a `model × tool × mode × engine × workload` grid. Use `--engine-map host=label,host=label` to override for hosts whose names don't follow the convention.
