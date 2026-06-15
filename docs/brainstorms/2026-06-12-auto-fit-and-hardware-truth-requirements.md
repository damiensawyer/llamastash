---
date: 2026-06-12
topic: auto-fit-and-hardware-truth
---

# Auto launch mode (fit delegation) + hardware truth layer

## Problem Frame

llamastash pins `n_gpu_layers=99` on every GPU launch (`src/launch/defaults_table.rs`) and emits an explicit `-c` when `src/launch/ctx_fit.rs` can compute one. Both choices actively disable llama-server's built-in `--fit` machinery, which only adjusts parameters the user left unset. Result: models bigger than the GPU pool OOM at load or silently degrade, instead of loading partially offloaded the way Ollama/LM Studio users expect (TODO v0.0.4-checklist items "Automatic gpu/cpu offload split" and "better strategy for finding the best models for a hardware").

ctx_fit was built on a claim ("llama.cpp `--fit` mis-reports unified-memory free space, collapses to the 4096 floor") that was true when written (commit `a967b0e`, 2026-05-25) but was fixed *underneath* llama.cpp by GPU-stack updates (kernel 7.0.10/11 + amdgpu firmware 20260519 + ROCm 7.2.4, May 28-Jun 4). Live validation on the same b9245 binary now shows `--fit` doing granular ctx reduction, binary-search layer placement, and sub-layer MoE expert offload (finer than `--n-cpu-moe` can express) in 0.7-3.2 s.

One genuine upstream weakness remains, proven live: on UMA/iGPU systems llama.cpp's "free memory" reading tracks system-available RAM, not the GTT pool hipMalloc actually allocates from. With one 44 GiB model resident, a second server saw "46 GiB free", projected a clean fit, and hard-OOM'd (GTT had ~16.5 GiB left). llamastash's own sysfs-based sampling (`src/gpu/amd.rs::combine_uma_memory`) was correct through *both* upstream failure regimes (May's under-report, today's over-report).

Separately, hardware reporting is inconsistent across surfaces: `status` shows only a GPU count, `doctor` has no hardware section and misses memory-size drift (this machine's GPU pool doubled, 64 → 124.5 GiB, with zero findings), and the TUI host pane on UMA boxes shows two nearly identical totals (`RAM* 25/125G`, `VRAM 3.0/124G`) that read as ~249 G on a 128 G machine. On UMA systems the kernel-default GTT cap (≈50% of RAM) also silently halves the usable GPU budget, and nothing tells the user that ceiling is a kernel config default rather than hardware.

## Requirements

**Workstream 1 — Auto launch mode**

- R1. Every typed knob gains a third value state, `Auto`, alongside `Inherited` (today's "Default": resolve through preset/last-used/arch layers) and explicit `Set`: cyclable in the TUI knob selector, accepted as the literal `auto` on each CLI flag, and a config/env/flag selects the default launch mode (factory value: Auto, per R2). A keybinding flips the whole launch editor to Auto as a reversible toggle that snapshots and restores the prior knob states. Fit-governed knobs render as `Auto (fit)`, others as plain `Auto`; the help-overlay legend explains the split.
- R2. Auto is the out-of-box default for launches. `defaults_table.rs` stops pinning `n_gpu_layers=99`; the wildcard and per-arch GPU rows resolve to Auto instead. Explicit user-set values always win. Auto is a knob *state*, not a resolver layer: a knob in Auto skips the Inherited layers entirely. The last-params recorder persists only user-set knobs going forward, and a one-time migration drops the previously auto-injected `ngl`/ctx values from existing `last_params` (pre-announcement, breaking is free — without this, every existing install's persisted `ngl=99` would silently pin the old regime). The default flips on all platforms in code; shipping it is gated per platform by the existing UAT smoke matrix + the R10 benchmark.
- R3. Auto semantics: emit nothing for that flag and delegate the decision to llama-server `--fit`. `Inherited` keeps today's layered meaning. For knobs `--fit` does not govern (threads, cache types, batch sizes, ...), Auto degenerates to "llama-server default"; the `Auto (fit)` vs `Auto` rendering (R1) keeps the two meanings distinguishable.
- R4. llamastash remains the memory-budget authority via **admission control + a reservation ledger**, not by overriding fit's numbers everywhere. Before spawning, llamastash projects demand (existing `src/gguf/memory.rs` estimators) against its own sampled budget — sysfs VRAM+GTT on UMA, vendor tools on dGPU, **and system RAM for CPU-resident portions** (upstream fit explicitly assumes system memory is unlimited). In-flight launches reserve their projected peak in a ledger so concurrent auto-starts cannot double-book the pool (the 1 Hz host-metrics sampler alone lags allocation by seconds; proxy auto-start serializes per-model only). On UMA — where upstream's free reading is proven wrong — llamastash additionally translates its budget into a `--fit-target` margin; on dGPU it passes only a default margin and trusts upstream's reading. Longer-term: propose an absolute per-device budget flag upstream so the margin translation can retire.
- R5. Auto passes a ctx floor via `--fit-ctx`, default 16384, configurable (config + env). Under memory pressure fit then sacrifices layers before dropping context below the floor (upstream's 4096 default is what produced the "useless tiny ctx" complaints). When even the floor cannot be satisfied, R19's degradation policy applies — never silent.
- R6. Post-launch actuals: after load, surface what fit actually chose (resolved ctx, layers on GPU, expert-tensor placement) in the `start` command output (one-line post-load summary), the TUI Running view, `show`, and `status`. Pre-launch prediction via the bundled `llama-fit-params` is explicitly a follow-up, not v1.
- R7. Retire ctx_fit from the GPU launch path (fit owns ctx under Auto; user-pinned ctx is respected by fit). Keep the `src/gguf/memory.rs` estimators for display, recommender, and budget math. Delete the stale mis-report claim from the module docs. Carve-outs: ctx_fit's RAM-budget ctx cap survives inside R4's admission check for CPU-only hosts and heavily CPU-spilled launches (fit does not budget system RAM), and the legacy defaults path survives behind R8's gate.
- R8. Gate Auto on a fit-capable llama-server build. On builds without `--fit`, the legacy defaults path (`ngl=99` rows + ctx_fit) is retained behind the gate as an explicit carve-out — not "today's Default behavior", which R2/R7 otherwise remove. Affected knobs render `Auto (unavailable)`, and the degradation is surfaced in `start` output, the TUI status line, and a doctor finding.
- R9. Auto applies to the llama.cpp backend only. Lemonade-delegated rows keep today's behavior, where knobs the backend can't honor are hidden outright (`src/tui/launch_picker.rs` `field_visible`); if Auto should instead be visibly unavailable on those rows, that is new greying behavior to design, not an existing pattern.
- R10. Validation includes a benchmark: Auto vs today's `ngl=99` status quo vs hand-tuned configs (LLM-BENCH baselines), across dense + MoE models, fits-fully + oversized cases, on the local hardware matrix. Plus a regression test for the concurrent-model scenario above: second model must load partially offloaded or be refused by admission control, not OOM. The fit smoke + concurrent regression also rerun as part of the managed llama-server upgrade/qualification path (fit behavior changes under a pinned binary — the May regression proved it). The benchmark validates the 16384 ctx-floor default (throughput at the floor on oversized cases) before it ships.

**Workstream 2 — Hardware truth layer**

- R11. Live detection is the single displayed truth: `status`, `doctor`, the init banner, and the TUI host pane render the same freshly-built hardware snapshot (the R12 field set). `init_snapshot.json` stays what it is — a receipt/baseline (managed-key ownership digests, binary digest, hardware baseline) — and is never rendered as if it were current hardware.
- R12. `doctor` gains a hardware section: CPU brand/cores, memory, disk free, per-GPU device rows with backend flavor, and on UMA boxes the pool composition (carve-out + GTT) with the effective ceiling. The init banner shows the same field set, sourced from the shared snapshot so the surfaces cannot drift.
- R13. `doctor` gains a memory-drift finding: GPU pool size materially changed vs the init baseline surfaces as a finding recording old → new (growth = info; shrinkage = warning, since a shrunk pool will start OOMing previously-fitting launches) and refreshes the baseline automatically. (Vendor-change drift already exists in `src/init/doctor.rs`; size drift is currently invisible. This gives doctor a narrow write path — today it is documented as strictly read-only, an acknowledged contract change.)
- R14. GTT hint: on Linux UMA systems running the kernel-default GTT cap (≈50% of RAM), `doctor` suggests raising it via `ttm.pages_limit`/`amdgpu.gttsize` kernel params with a docs link. Never auto-apply. Do not recommend `amd_iommu=off` (verified to break TB4 docking on the reference machine; perf claim is situational).
- R15. Rename `RAM` → `MEM` and `RAM*` → `MEM*` across all surfaces (TUI host pane, help-overlay legend, init banner, `status`, the R12 doctor section, docs). On UMA boxes the GPU-memory row is labelled `GPU (shared)` and sits directly under the `MEM*` row — two rows, clearer labels; the asterisk keeps exactly one legend meaning (unified pool). Bare `VRAM*` was rejected: an asterisk alone does not stop the two ~125G bars reading as additive.
- R16. One UMA headroom policy across vendors — **decided: raw display, headroom in admission**. All surfaces display true raw pool totals; the usable-fraction policy (Apple's current 0.75× in `aggregate_vram_bytes` at `src/init/detection.rs`, AMD's implicit none) moves into R4's admission check, applied uniformly per pool type. Display stays truthful, policy is centralized in one place. (R17 was merged into R11/R12.)
- R18. UMA classification must rest on an explicit integrated-GPU signal (e.g. the driver/PCI integrated flag), not the current `gtt_total > vram_total` heuristic (`src/gpu/amd.rs::combine_uma_memory`). The heuristic misclassifies any discrete AMD card with VRAM < RAM/2 (kernel-default GTT is ~50% of RAM regardless of card type), summing a phantom pool — harmless as today's display bar, catastrophic once R4 budgets against it.
- R19. Degraded-placement policy: after an Auto launch, llamastash compares actuals (R6) against full offload. Material degradation — many layers on CPU, ctx clamped at the floor, or outright fit failure (llama-server loads best-effort even when fit *fails*; verified upstream) — produces a launch-time notice on the launching surface. A config option provides strict mode: refuse/stop instead of running degraded, for users who would rather pick a smaller model than crawl.

## Budget/placement split (the core design)

```
llamastash (budget authority)                llama-server (placement engine)
  sysfs VRAM+GTT (+ RAM) sampling              --fit solves: ctx reduction,
  admission check + reservation ledger         layer split, MoE expert
  ctx floor policy             ── spawn ──►    placement (sub-layer -ot)
  (--fit-ctx; UMA-only --fit-target margin)           │
                                                      ▼
  start / TUI / show / status  ◄──── post-launch actuals (R6)
        └─ degradation notice / strict mode (R19)
```

## Success Criteria

- An oversized model on a GPU host loads partially offloaded with ctx ≥ the floor where the budget allows; when even that is impossible, the outcome is an explicit notice or strict-mode refusal (R19) — never a silent all-CPU crawl, an OOM, or a 4096 collapse.
- The proven concurrent-load failure (44 GiB resident + 37 GiB second model on a ~60 GiB pool) ends in a partial-offload load or an explicit admission-control refusal — not a crash.
- Benchmark (R10): Auto launches are within an agreed margin of hand-tuned throughput on the matrix and produce zero OOMs; results recorded in `docs/benchmarks*`.
- Every surface (TUI, `status`, `doctor`, init) shows the same hardware numbers; UMA boxes show `MEM*` and a shared-pool GPU row; a GPU-pool size change produces a doctor finding on the next run.
- After an Auto launch, the resolved ctx / GPU layers / expert placement are visible in the TUI Running view, `show`, and `status` (R6).
- On a non-fit-capable binary, launches still succeed and a visible degradation notice appears (R8).
- `doctor` renders the hardware section on all supported platforms (R12).
- Apple and AMD UMA hosts report usable GPU memory under the same headroom rule (R16).

## Scope Boundaries

- No in-house placement solver (per-layer fitting, expert assignment) — upstream `--fit` owns placement; we own the budget. The benchmark (R10) is the tripwire that revisits this if fit underperforms.
- No pre-launch fit preview in v1 (R6 follow-up).
- Topic A — "better strategy for finding the best models for a hardware" — is a separate brainstorm that consumes this one's outcome (partial offload turns the recommender's binary fits/doesn't-fit into a tiered signal).
- No auto-applying kernel parameters or BIOS guidance beyond the doctor hint (R14).
- Windows `--fit` + VRAM-swap behavior is validated in the benchmark phase, not specially engineered for in v1.
- Within workstream 2, only R16 blocks workstream 1 (the headroom rule feeds the `--fit-target` formula); R11-R15 are independently shippable display/diagnostic work.

## Key Decisions

- **Delegate placement to `--fit`, keep budget authority**: live tests showed upstream fit is allocation-grade (granular ctx, sub-layer MoE expert offload) — re-implementing it would be strictly worse — but its UMA free-memory reading conflates pools (proven OOM), and llamastash's sysfs accounting was right through both historical failure regimes.
- **Budget enforcement = admission control + reservation ledger**: `--fit-target` is only a *margin over upstream's own free reading* (verified in `common/fit.cpp`), so it cannot carry an absolute budget; enforcement has to happen before spawn, in llamastash. Margin translation is a UMA-only assist, not the mechanism.
- **Auto default-on, release gated by UAT + benchmark**: pre-announcement, breaking-change cost is zero, and the current `ngl=99` default is what causes the OOM/all-CPU problem; but fit is live-validated on Linux/ROCm only, so the per-platform UAT smoke matrix + R10 gate the release, not the code path.
- **Auto is a state, not a layer; one-time last_params wipe**: the recorder persisted *resolved* knobs (`ngl=99`, computed ctx) which outrank arch defaults — without the wipe and recorder fix, Auto would silently never activate on any machine that ever launched a model.
- **All knobs get Auto, rendered as `Auto (fit)` vs `Auto`**: uniform cycling UX beats a special-cased subset, but the two meanings (fit-solved vs server-default) must be visually distinct or the state is unexplainable.
- **"Inherited" rename**: once Auto is the actual default, a state named "Default" lies about what happens when you touch nothing.
- **ctx floor 16384**: agent/coding workloads (the product's positioning) break below ~16k; matches the recommender's `DEFAULT_CTX`; R10 validates the default empirically before ship.
- **Retire ctx_fit from the GPU path, keep estimators + RAM guard**: its premise (broken upstream fit) is gone, but upstream fit assumes system RAM is unlimited, so ctx_fit's RAM-budget role survives inside admission control.
- **Post-launch actuals only in v1**: avoids `llama-fit-params` stdout coupling and 1-3 s pre-launch latency while still making Auto's outcome visible.
- **MEM/MEM* rename + `GPU (shared)` row**: the current `RAM*`/`VRAM` pair displays ~2× a UMA machine's physical memory; the owner himself misread it. Two rows with clearer labels chosen over sub-gauges (minimal rendering change, one asterisk meaning).

## Dependencies / Assumptions

- Managed llama-server builds at or above the fit-capable release (b9245 verified; R8 gates older builds).
- Fit behavior validated live on Linux/ROCm (Strix Halo). CUDA/Metal/Vulkan/Windows parity is assumed pending the R10 benchmark; upstream reports of Windows VRAM-swap degradation are unverified.
- The UMA overcommit failure mode persists upstream (tracked as llama.cpp #22592-class issues); if upstream fixes its UMA accounting, R4 becomes belt-and-suspenders rather than load-bearing.

## Outstanding Questions

### Resolve Before Planning
- (none — R16 resolved: raw display, headroom centralized in R4's admission check)

### Deferred to Planning
- [Affects R6][Technical] Source for post-launch actuals: fit's decision lines may be trace-level at default verbosity, so `/props`/`/slots` (or a `-lv` bump on the child) is likely the viable source — verify per build.
- [Affects R8][Technical] Fit-support detection: minimum release tag vs `--help` probe vs trying `--fit` and watching for unknown-arg failure; and whether llamastash refuses to manage builds older than the fit floor or carries the legacy table indefinitely.
- [Affects R4][Technical] Admission-control demand estimate shape (projected peak vs weights+KV+overhead band), reservation ledger lifecycle (reserve on spawn, settle on Ready/Error), the UMA margin-translation formula, and load-time re-validation against unmanaged GPU consumers (user-run llama-server, Lemonade/FLM, desktop use share the same pool).
- [Affects R4][Needs research] Upstream proposal for an absolute per-device budget flag (supersedes margin translation when accepted).
- [Affects R2][Technical] Migration mechanics for the one-time `last_params` wipe (schema bump vs targeted field drop) and presets that captured explicit `ngl=99`.
- [Affects R1][Technical] Whether the literal `auto` collides with any existing CLI value parsing (numeric clap args; string knobs where `auto` could become a legal upstream value).
- [Affects R15][Technical] Full sweep of rename touchpoints (TUI panes, help, init banner, `status`, docs, tests asserting on `RAM*`).
- [Affects R10][Needs research] Benchmark margin definition ("within X% of hand-tuned") and which LLM-BENCH baselines map cleanly to Auto-comparable runs.

## Next Steps
-> `/ce:plan` for structured implementation planning.
