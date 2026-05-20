---
title: "feat: Live HF Hub snapshot discovery (whichllm-style)"
type: feat
status: completed
date: 2026-05-20
origin: conversation 2026-05-19/20 (per-backend overhead band investigation → catalog refresh discussion)
---

# Live HF Hub snapshot discovery (whichllm-style)

## Overview

Replace `data/benchmark-snapshot.json`'s 12-row hand-curated catalog with one
that self-refreshes daily in CI from live HuggingFace Hub queries, following
whichllm's discovery pattern (downloads + lastModified + trending + explicit
frontier list). The bundled JSON keeps acting as the binary's `include_str!`'d
ground truth; the regen script becomes the catalog *owner*, not just the score
refresher.

Today's catalog is roughly a year stale relative to the open-weight frontier
— Qwen3.6, Gemma 4, DeepSeek V3.2/V4, GLM-5, Kimi K2, Llama 4, Phi-4 are all
absent. whichllm covers them because it queries HF on every invocation; we
already vendor whichllm partially for benchmark scores (per
`docs/plans/2026-05-19-001-feat-vendor-benchmark-scrapers-plan.md`), so
extending that vendoring to *model discovery* is the consistent move.

## Scope

**In scope:**

- Adopt whichllm's `fetch_models()` (via pip, pinned to the same release as
  our vendored shim commit) into `scripts/regenerate-benchmark-snapshot.py`
  as the catalog source.
- Filter whichllm's output to "has a Q4_K_M (or equivalent) GGUF on HF
  from an allowlisted publisher" before adding to the snapshot — the
  bundled catalog only ships models llamastash can actually launch.
- Add `source_hf_id`, `params_active`, `is_moe`, `gguf_publisher` to the
  snapshot schema and the Rust `ModelEntry` struct.
- Port whichllm's MoE-aware KV / activation math into
  `src/init/recommender.rs::estimate_peak_bytes` so models like
  Qwen3-Next-80B-A3B fit correctly.
- Bump the bundled-snapshot size budget from 500 KiB to 2 MiB (still
  ~0.05% of the binary).
- Rotate `tests/recommender_corpus.rs` to a predicate-based `expected`
  (any model with the right `task_hints` whose params fits the cell)
  rather than hardcoded ids.
- Drop `BUNDLED_ID_TO_SOURCE_HF_ID` — whichllm gives the base HF id
  directly, so the join table goes away.

**Out of scope:**

- Recommender ranker / composite-score weight changes (separate concern).
- Vision / multimodal models (whichllm's `include_vision=True` path stays
  off for v2).
- UMA / APU detection (separate work tracked in TODO; the catalog refresh
  doesn't unblock or block it).
- Live HF discovery *at recommender runtime* — R45 single-binary invariant
  stays. Discovery only happens in CI.
- Replacing the daily-cron / GitHub Releases publish pipeline — Unit 7's
  framework stays as-is, we just put more in the artefact it produces.

## Problem Frame

- `data/benchmark-snapshot.json::models[]` lists Qwen 2.5 + Llama 3.x +
  Mistral Nemo 12B. No Qwen3, no Gemma family, no DeepSeek, no GLM, no
  Phi, no Llama 4.
- whichllm's `_FRONTIER_MODEL_IDS` and trending-sort paths surface ~60
  current-frontier models we miss.
- The current regen flow refreshes *scores* for the 12 rows but won't
  ever discover new models — a maintainer has to PR each new entry.
- The recommender's estimator
  (`weights × 1.20 + weights × 0.15 × ctx_scale`) assumes dense models;
  MoE models like Qwen3-Next-80B-A3B (80B total / 3B active) blow it
  either way (over-reserves on weight, under-reserves on KV).

## Key Decisions

**Adoption pattern: pip-install whichllm in CI, pin to a tag.** We already
vendor whichllm partially. Going from "vendored shim" to "pip dep" in
the CI environment is a smaller lift than copying ~1000 lines of
`fetcher.py`. R45 is unaffected because the binary doesn't gain a
Python dep — only the CI regen step does. Pin via `whichllm==0.5.7` (or
whatever tag corresponds to the vendored commit) in
`scripts/requirements.txt`. The vendored shim's
`WHICHLLM_VENDORED_COMMIT` constant moves in lockstep.

**No live discovery at runtime.** Bundled snapshot remains
`include_str!`'d. Discovery happens once a day in CI. Users get the
freshness via the remote-snapshot fetch path (`snapshot-latest` GH
Release asset).

**Catalog cap: 100 rows.** Roomy enough to cover every corpus tier
with multiple picks per (task, family) bucket including MoE flagships
and frontier additions like DeepSeek V3.2 / GLM-5 / Llama 4. JSON
lands around 330 KiB with the new schema fields. Within each (tier,
task) bucket, rank by composite score (benchmark × 0.5 + recency × 0.3
+ downloads × 0.2) and take the top entries. The corpus refactor in
Unit 5 needs to be tested carefully for rotation stability at this
size.

**`BUNDLED_ID_TO_SOURCE_HF_ID` goes away.** whichllm gives the base
HF id directly. The join becomes "match the bundled row's
`source_hf_id` field against the adapter's score map". The standalone
join table dies.

**`task_hints` stay maintainer-curated, in a side file.** Inference
from model name ("coder" → "code", "thinking" → "reasoning") is brittle
and silently mis-classifies models like `Mistral-Small-3.2` (which the
upstream calls general but ships coder-strong evals). Better:
`data/task-hints.yaml` maps HF id (or family prefix) → tag list, and
the regen merges that into each row. Unknown models get
`["general"]` as a default. The corpus gate fails if a task cell
loses all its picks, forcing a maintainer to add the missing hint.

**MoE accounting goes in the recommender, not just the snapshot
consumer.** Add `params_active: Option<u64>` + `is_moe: bool` to
`ModelEntry`. Port whichllm's `_KV_BYTES_PER_BPARAM_PER_KCTX = 3.5 MB`
and `_MOE_ATTENTION_PARAM_MULTIPLIER = 4.0` into
`estimate_peak_bytes`. Keep the current dense path for backward
compatibility (`is_moe = false` defaulted on parse).

**GGUF variant selection: prefer official-org GGUF, fall back to
allowlist.** Many frontier models don't publish official GGUFs
(e.g., Llama 4). For those, accept GGUFs from publishers named in
`data/gguf-publisher-allowlist.yaml` (`bartowski`, `unsloth`,
`lmstudio-community`, `QuantFactory`, `mradermacher`, plus the
model's own org). Reject everything else to avoid prompt-injected
manifests reaching users.

**Corpus rotates with the catalog.** Each corpus row's `expected`
becomes a predicate `(task, max_params_or_active_b, max_ctx)` rather
than a fixed list of ids. Expected picks are computed at test time
from the snapshot. Catches "the catalog rotated and nothing fits the
6 GB Nvidia cell anymore" as a corpus failure rather than a silent
recommender regression.

**Schema stays at `schema_version: 1`.** llamastash hasn't shipped a
stable release with the snapshot format yet — there are no v1 binaries
"in the wild" to be compatible with. New fields land directly in the
existing schema. Serde `#[serde(default)]` is still used on the new
fields so in-tree fixtures and future evolution stay forgiving, but we
don't pay the v1/v2 dual-deserialization cost up front. Bump
`schema_version` to 2 only when a real backward-incompatible change
arrives post-release.

## High-Level Technical Design

```
                     ┌───────────────────────────────────────────┐
                     │  scripts/regenerate-benchmark-snapshot.py │
                     └────────────────────┬──────────────────────┘
                                          │
              ┌───────────────────────────┴────────────────────────┐
              ▼                                                    ▼
   ┌──────────────────────┐                            ┌──────────────────────┐
   │ whichllm.fetch_models│                            │  benchmark adapters  │
   │ (live HF query +     │                            │  (open_llm_lb +      │
   │  frontier list)      │                            │   aider, today)      │
   └──────────┬───────────┘                            └──────────┬───────────┘
              │                                                   │
              │  ~80 candidate ModelInfo                          │  id → score
              ▼                                                   │
   ┌────────────────────────────┐                                 │
   │  filter to has-Q4_K_M-GGUF │                                 │
   │  + publisher allowlist     │                                 │
   └────────────┬───────────────┘                                 │
                │ ~50 candidates                                  │
                ▼                                                 │
   ┌────────────────────────────┐    data/task-hints.yaml ──┐     │
   │  attach task_hints +       │◀─────────────────────────┘      │
   │  rank within (tier, task)  │                                 │
   └────────────┬───────────────┘                                 │
                │ top-60                                          │
                ▼                                                 │
   ┌────────────────────────────────────────────────────────────┐ │
   │                  build_snapshot()                          │◀┘
   │  - merge scores                                            │
   │  - preserve recommender_weights (unchanged)                │
   │  - emit schema v1 + params_active / is_moe (new fields)    │
   └────────────────────┬───────────────────────────────────────┘
                        │
                        ▼
              corpus gate (16/20)  ─── fail ──▶ exit non-zero, no publish
                        │
                       pass
                        ▼
       write candidate / publish to GH Release
```

## Units

### Unit 1 — Schema extension + Rust-side parsing

- Add to `ModelEntry`: `source_hf_id: String`, `params_active:
  Option<u64>`, `is_moe: bool` (`#[serde(default)]`), `gguf_publisher:
  String`. `#[serde(default)]` on the new fields keeps in-tree
  fixtures and future evolution forgiving.
- Keep `SCHEMA_VERSION = 1` (pre-release, no compat burden — see Key
  Decisions).
- Update `src/init/benchmark.rs` deserializer.
- Update fixture in `tests/init_snapshot_persistence.rs` to assert the
  new fields round-trip correctly.
- **Doneness:** the existing 12-row bundled snapshot still parses with
  the new fields defaulted (since the JSON doesn't carry them yet),
  and a hand-rolled snapshot with the new fields explicit also parses.
  Existing recommender tests pass unchanged.

### Unit 2 — MoE-aware estimator

- Port whichllm's `_KV_BYTES_PER_BPARAM_PER_KCTX = 3.5 MB` and
  `_MOE_ATTENTION_PARAM_MULTIPLIER = 4.0` into
  `src/init/recommender.rs::estimate_peak_bytes`.
- Branch on `entry.is_moe`: dense path unchanged; MoE path uses
  `params_active × MoE multiplier` for the KV term, and
  `weights × 1.20` for activations (weights still all resident — MoE
  saves on compute, not memory).
- Add inline unit tests covering:
  - Qwen3-Next-80B-A3B (80B total, 3B active) at ctx=4k/16k/32k
  - Qwen3-Coder-30B-A3B at ctx=4k/16k
  - dense regression: existing Qwen2.5-7B numbers unchanged
- **Doneness:** estimator returns within ±15% of whichllm's
  `estimate_vram` for the same model + ctx.

### Unit 3 — Wire whichllm into the regen script

- Add `whichllm==0.5.x` to `scripts/requirements.txt`.
- New module `scripts/benchmark_sources/hf_discovery.py`:
  - Wraps `whichllm.models.fetcher.fetch_models()`.
  - Applies GGUF-availability filter (requires at least one `.gguf`
    sibling matching `Q4_K_M`, `Q4_K_S`, `Q5_K_M` per quant).
  - Applies publisher allowlist (see Unit 4).
  - Yields `(source_hf_id, repo, file, params, params_active,
    is_moe, weights_bytes, gguf_publisher)` tuples.
- Extend `scripts/regenerate-benchmark-snapshot.py::build_snapshot` to:
  - Pull candidates from `hf_discovery`.
  - Merge with adapter scores (existing flow).
  - Attach task hints (Unit 4).
  - Rank within (tier, task) bucket and cap to 100 rows.
- Drop `BUNDLED_ID_TO_SOURCE_HF_ID`.
- **Doneness:** `python scripts/regenerate-benchmark-snapshot.py
  --dry-run` produces a candidate snapshot with ≥40 rows including
  Qwen3.6-27B, Gemma 4 31B, DeepSeek V3.2, GLM-5, Phi-4.

### Unit 4 — task-hints curation file + publisher allowlist

- New `data/task-hints.yaml`:
  ```yaml
  # Maps HF id prefix or family substring → task_hints tag list.
  # Longest match wins. Unmatched models default to ["general"].
  prefixes:
    "Qwen/Qwen3-Coder": ["code"]
    "Qwen/Qwen3.6-Coder": ["code"]
    "Qwen/QwQ": ["reasoning"]
    "deepseek-ai/DeepSeek-R1": ["reasoning", "general"]
    "deepseek-ai/DeepSeek-V3": ["general", "reasoning"]
    "google/gemma-4": ["general", "reasoning"]
    "mistralai/Codestral": ["code"]
    "mistralai/Devstral": ["code"]
    "microsoft/Phi-4-mini": ["general", "code"]
    # ... (full list in implementation)
  defaults: ["general"]
  ```
- New `data/gguf-publisher-allowlist.yaml`:
  ```yaml
  # HF orgs whose GGUF repos we trust. The model's own org is
  # always allowlisted implicitly.
  - bartowski
  - unsloth
  - lmstudio-community
  - QuantFactory
  - mradermacher
  - TheBloke  # legacy but still hosts good quants for older models
  ```
- Build-time check in `build.rs`: parse both YAMLs to ensure
  they're well-formed before the snapshot `include_str!`.
- **Doneness:** the hints/allowlist are documented in the
  regenerator's docstring; a doctor finding surfaces if either YAML
  fails to parse.

### Unit 5 — Corpus rotation strategy

- Refactor `tests/recommender_corpus.rs::Case`:
  - Replace `expected: &'static [&'static str]` with
    `expected: ExpectedFit { task: Option<&'static str>,
                              max_params_b: f32,
                              prefer_moe: bool }`.
  - Helper: "for cell (vram=X, task=Y, ctx=Z), at least one model in
    the top-3 must (a) have `task_hints` containing Y and (b) have
    `params ≤ max_params_b × 1e9` (or, if `prefer_moe`,
    `params_active ≤ max_params_b × 1e9`)".
- Keep the 16/20 ratio; failure messages quote which cells failed
  and the top-3 actually returned.
- **Doneness:** corpus passes against the new 60-row catalog and
  *fails* loudly if a manual snapshot tweak drops a task tier or
  if the auto-regen ends up rotating away from a fitting model.

### Unit 6 — Size-budget bump + docs

- Update `build.rs` (or wherever the snapshot `const_assert` lives —
  see Unit 5 of plan `2026-05-18-001`) to raise the bundled
  snapshot ceiling from 500 KiB to 2 MiB.
- Patch the 500 KiB line in `docs/plans/2026-05-18-001-feat-init-
  wizard-doctor-pull-plan.md::Patterns` with a `(superseded by Unit
  6 of plan 2026-05-20-001)` note.
- Update `scripts/benchmark_sources/README.md` with the new
  pipeline description and the pip-install requirement.
- (No `schema_version` bump — see Key Decisions; stays at 1.)
- **Doneness:** `git grep "500 KiB"` returns only historical
  references with explicit supersession notes.

### Unit 7 — CI: HF token + lockstep version assertion

- Add an `HF_TOKEN` secret to the snapshot-regen GitHub Actions
  workflow. whichllm's anonymous fetch hits HF Hub rate limits on
  the 5-7 query pattern under repeat runs (issues observed during
  whichllm's own CI).
- Add a CI lint step:
  ```bash
  python -c "
  from scripts.benchmark_sources.whichllm import (
      WHICHLLM_VENDORED_COMMIT, WHICHLLM_VENDORED_DATE)
  import whichllm
  # assert in-tree shim's version matches installed package
  assert whichllm.__version__.replace('.', '') in WHICHLLM_VENDORED_COMMIT or \
         WHICHLLM_VENDORED_DATE  # date check fallback
  "
  ```
  More robust: encode the expected `whichllm.__version__` directly
  in the shim and assert exact match.
- **Doneness:** the regen workflow's daily run passes both the
  HF-quota check and the version-lockstep assertion.

## Open Questions

- **whichllm API stability.** Pinning to `whichllm==0.5.7` matches
  our vendored commit, but `fetch_models` could shift in a 0.6
  release. Bumping is a manual decision; we don't auto-update.
- **HF rate limits in CI.** Set `HF_TOKEN` in the Actions secret
  store (covered in Unit 7).
- **MoE families with unstated `params_active`.** Some models don't
  publish active-param counts in HF metadata; whichllm has curated
  fallbacks in `_resolve_moe_active_params`. Unit 3 imports those
  alongside the fetcher; if a new MoE arrives without a fallback,
  it's flagged in CI output and a maintainer adds the entry.
- **Snapshot-budget overrun.** 2 MiB is conservative for ~100 rows
  but may need re-evaluation once we see actual JSON size from a
  full regen run. The build-time `const_assert` catches an
  overrun and we bump deliberately.
- **Recency / churn UX.** A user who runs `llamastash init` today and
  again after a CI refresh may see different top-3 picks. The
  on-disk tiebreak (R60) keeps already-downloaded models pinned, so
  churn affects fresh installs more than maintenance runs.

## Risks

- **Catalog churn breaks user expectations.** Mitigation: R60
  on-disk tiebreak keeps already-downloaded models pinned.
- **Snapshot size bloat past 2 MiB.** Build-time `const_assert`
  catches it; bump deliberately.
- **whichllm bug surface.** Adopting a third-party CI dep means we
  inherit its bugs (e.g., HF metadata-extraction regression). The
  corpus gate is the safety net; if a whichllm bug flips picks, the
  daily CI fails to publish and a maintenance issue is auto-filed.
- **Pinned commit drift.** The vendored shim's
  `WHICHLLM_VENDORED_COMMIT` and the pip-pinned version must stay
  in lockstep. Unit 7's CI lint asserts this.
- **HF Hub outage.** If `whichllm.fetch_models()` fails, CI must not
  publish a stripped catalog. Mitigation: pre-existing partial-
  source-failure policy in `build_snapshot` (publish only if every
  source returned data) covers this — the bundled snapshot stays
  live until the next successful run.

## Post-plan refinements (2026-05-20)

After the seven planned units shipped, an audit comparing
`llamastash init --only models --json` against
`whichllm --json --top 10` revealed the recommendations had drifted —
the top-3 surfaced two-generation-old Qwen 2.5 picks on a 64 GB box
where whichllm picked Qwen3-30B-A3B / gemma-4-31B-it / gpt-oss-120b.
Five follow-up commits landed to close most of the gap while keeping
the architecture (snapshot + Rust runtime, no Python at user runtime)
intact:

1. **`refactor(snapshot): delegate benchmark scoring to whichllm`**
   (`2dc70ff`) — collapsed 623 lines of vendored adapters
   (`open_llm_leaderboard.py`, `aider.py`) into one
   `whichllm_combined.py` that calls
   `whichllm.models.benchmark.fetch_benchmark_scores()`. Inherits all
   six upstream sources (OLLB v2, Chatbot Arena, LiveBench, AA Index,
   Aider, Vision) + layered current-over-frozen merge + lineage
   recency demotion. Fixes the root cause: we'd shipped only 2/6 of
   whichllm's score pipeline and the 40.0/no-source floor swallowed
   every modern release.
2. **`feat(init): richer hardware banner`** (`247f848`) — extended
   `HardwareSnapshot` with `cpu_brand`, `cpu_cores`,
   `cpu_features` (AVX2 / AVX-512 / FMA / NEON / SVE),
   `disk_free_bytes`; reworked the "detected: …" line into three
   grouped segments (`gpu:` / `cpu:` / `sys:`).
3. **`feat(init): top-10 recommendations under --json, lift picker
   to top-10`** (`0f89edd`) — bumped `DEFAULT_TOP_N` 5 → 10 to match
   whichllm's default and added an `InitSummary.recommendations`
   field so `init --only models --json` works as a listing surface
   without triggering downloads. Lets maintainers diff our output
   against `whichllm --json --top 10` directly via `jq`.
4. **`feat(snapshot): emit one snapshot row per preferred quant`**
   (`58ee985`) — hf-discovery now produces one row per available
   preferred quant per model (Q3_K_M / Q4_K_S / Q4_K_M / Q5_K_M /
   Q6_K / Q8_0) instead of just Q4_K_M. Snapshot budget changed from
   "100 rows" to "all preferred quants of the top 250 unique source
   models" (~1500 rows, ~840 KiB). Per-quant quality discounts +
   speed multipliers added to the regen so the composite ranker can
   distinguish quants within a family. Recommender output dedup
   keeps user-facing top-N as one row per `source_hf_id` (best-fit
   quant wins).
5. **`feat(snapshot): match whichllm catalog + ranking on this
   hardware`** (`c80d638`) — three coupled changes:
   - **Variant synthesis for official-org repos.** Ported whichllm's
     `_synthesize_variants_for_official_repo`: when an official-org
     candidate (Qwen / google / meta-llama / openai / zai-org / …)
     ships only safetensors, synthesize Q3-Q8 GGUF rows with file
     sizes estimated from `params × bytes_per_weight`. Brings
     `Qwen/Qwen3.6-27B`, `google/gemma-4-31B-it`, GLM-4.5-Air,
     Qwen3-Next-80B-A3B-Instruct, Llama-4-Scout into the catalog
     (community converters reliably publish GGUFs within days of
     official release).
   - **VRAM estimator port.** Replaced `weights × 1.20 + weights ×
     0.15 × ctx_scale` with whichllm's
     `weights + KV(3.5 MB/B/K) + activation(400 MB + 0.08 B/param +
     150 MB/4K)`. Old formula treated the entire weights file as
     activation overhead, overshooting MoE peaks 5-10× and gating
     gpt-oss-120b / GLM-4.5-Air off the recommender's list.
   - **Score tuning.** Recommender weights rebalanced to
     `bench=0.65, tok_per_second=0.05, param_quality=0.25,
     recency=0.05` so quality dominates as it does in whichllm.
     `params_quality_curve` anchor 14B → 80B keeps rewarding bigger
     models past 14B. Per-quant quality discounts moved to
     whichllm's `QUANT_QUALITY_PENALTY` table verbatim. Profile
     filter (drop coder / math / vision specializations when no
     `--task` is set) ported from
     `_matches_profile("general")`.

### Result vs whichllm on a 64 GB shared-VRAM box (AMD Strix Halo)

After the refinements, 7 of whichllm's top-10 models appear in our
top-10, with three quants matching exactly:

| # | whichllm | llamastash | Same model? | Same quant? |
|---|---|---|:-:|:-:|
| 1 | Qwen3-Next-80B-A3B-Instruct Q5_K_M | Qwen3.6-27B Q8_0 | — | — |
| 2 | Qwen3.6-27B Q3_K_M | gemma-4-31B-it Q8_0 | — | — |
| 3 | gpt-oss-120b Q3_K_M | Qwen3-Next-80B-A3B-Instruct Q5_K_M | ✓ | ✓ |
| 4 | Qwen3-30B-A3B Q6_K | gpt-oss-120b Q3_K_M | ✓ | ✓ |
| 5 | gemma-4-26B-A4B-it Q6_K | Qwen3-30B-A3B Q6_K | ✓ | ✓ |
| 6 | gemma-4-31B-it Q5_K_M | QwQ-32B Q8_0 | — | — |
| 7 | gpt-oss-20b Q6_K | Qwen3-Next-80B-A3B-Thinking Q5_K_M | — | — |
| 8 | GLM-4.7-Flash Q6_K | GLM-4.7-Flash Q8_0 | ✓ | — |
| 9 | GLM-4.5-Air Q4_K_M | DeepSeek-R1-Distill-Qwen-32B Q8_0 | — | — |
| 10 | Llama-4-Scout-17B-16E Q4_K_M | GLM-4.5-Air Q3_K_M | ✓ | — |

(Up from 4/10 model match and 0/10 quant match before the
refinements.)

### Remaining gap (deliberately not closed)

- **Family selection / lineage demotion.** Our extras (QwQ-32B,
  Qwen3-Next-80B-A3B-Thinking, DeepSeek-R1-Distill-Qwen-32B) are
  "reasoning / thinking / distill" variants that whichllm's
  `_generation_bonus` and family-grouping logic de-prioritize.
  Porting that subsystem is ~500 lines of `engine/ranker.py` and
  `models/grouper.py`; deferred because the extras are still
  *reasonable* picks for the user's box (just not the ones
  whichllm would surface).
- **Composite vs additive scoring.** whichllm's `_compute_quality_
  score` is an additive sum of bench × source_weight + size_score +
  speed_score + pop_score + source_bonus + gen_bonus +
  derivative_penalty (range ~0-100). Ours stays a weighted-average
  composite (range 0-1). The shape mismatch is why our quant picks
  skew larger (Q8 vs whichllm's Q5/Q6) even though the model
  rankings now mostly align. Porting the additive shape would
  require restructuring `composite_score` plus several upstream
  data fields we don't currently carry (popularity, generation
  index per family).
- **Per-host download fallback for synthetic rows.** Synthetic
  variants point at the source repo (which ships safetensors, not
  GGUF). `init --recommended` will fail to download them today. A
  follow-up would have the download flow try
  `bartowski/{name}-GGUF`, `unsloth/{name}-GGUF`,
  `lmstudio-community/{name}-GGUF` as fallback publishers when the
  source repo doesn't have the file.

### Files touched (post-plan)

- `scripts/benchmark_sources/whichllm_combined.py` (new)
- `scripts/benchmark_sources/whichllm.py` (attribution shim docstring)
- `scripts/benchmark_sources/hf_discovery.py` (multi-quant emission,
  variant synthesis, source_hf_id rule for official orgs, per-quant
  speed / quality mults, expanded preferred-quants list)
- `scripts/benchmark_sources/hf_discovery_test.py` (updated for
  multi-row returns)
- `scripts/benchmark_sources/README.md` (rewritten)
- `scripts/benchmark_sources/aider.py` (deleted, -299 lines)
- `scripts/benchmark_sources/open_llm_leaderboard.py` (deleted, -324
  lines)
- `scripts/regenerate-benchmark-snapshot.py` (collapsed scoring,
  per-quant mults applied in `_compose_model_entry`,
  `SNAPSHOT_MODEL_LIMIT` 100 → 250)
- `src/init/detection.rs` (new CPU/disk fields on `HardwareSnapshot`)
- `src/init/prompts.rs` (three-line banner)
- `src/init/recommender.rs` (whichllm-aligned `estimate_peak_bytes`,
  `params_quality_curve` anchor 14B → 80B, `profile_admits` filter,
  output dedup by `source_hf_id`, `DEFAULT_TOP_N` 5 → 10)
- `src/init/wizard.rs` (`recommendations` field on `InitSummary`,
  JSON-mode no-download branch in `run_models_step`)
- `tests/recommender_corpus.rs` (MoE-aware `prefer_moe=false` predicate)
- `src/init/smoke.rs`, test helpers across `src/init/{doctor,install,
  smoke}.rs` (defaults for new `HardwareSnapshot` fields)
- `data/benchmark-snapshot.json` (regenerated multi-quant, recommender
  weights updated)
- `Cargo.toml` (`sysinfo` `disk` feature)
