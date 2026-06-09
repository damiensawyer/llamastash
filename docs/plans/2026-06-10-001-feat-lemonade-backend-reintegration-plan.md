---
title: "feat: Lemonade backend re-integration (manual setup, no auto-install)"
type: feat
status: active
date: 2026-06-10
origin: docs/brainstorms/2026-06-08-multi-backend-abstraction-requirements.md
supersedes:
  - docs/plans/2026-06-09-001-feat-lemonade-backend-phase2-plan.md
  - docs/plans/2026-06-09-002-feat-lemonade-phase2b-plan.md
---

# feat: Lemonade backend re-integration (manual setup, no auto-install)

## Overview

The Lemonade engine was fully built on `feat/lemonade-backend-phase2` (original plans 001 +
002, Units 1–6 of each green; Unit 7 partial; Unit 8 deferred), then **stripped out** to land a
clean, llama.cpp-only **foundation** on `main` (the `Backend` seam, identity generalization,
persistence, status `backends`, backend-choice scaffold, picker/Settings gating). This plan
**re-applies** that engine onto the foundation, with one deliberate scope change:

- **No auto-install.** The original Unit 8 downloaded/extracted an embeddable `lemond` and wired
  it into `init`. We drop that entirely. The user installs Lemonade + its sub-backends manually;
  llamastash **finds** `lemond` (explicit path or `PATH`), **supervises** it as the umbrella, and
  documents the setup. (We still manage the process — we just never fetch it.)

This is not a blind cherry-pick: the foundation diverged from the side branch (genericized docs,
reverted `safe_extract`, picker gating finished). Each unit re-applies the engine code **onto the
current seam**, adjusting signatures where the foundation changed.

## What the foundation already provides (do NOT rebuild)

- `ModelIdentity { Gguf, Backend(BackendModelId) }` + `#[serde(untagged)]` persistence (state.json
  unchanged) — `src/backend/identity.rs`.
- `Lifecycle::{ProcessPerModel, ManagedMultiplexer}`, `LaunchPlan::{SpawnProcess, DelegateToManager}`,
  `ManagerLaunchSpec`, `ManagerModelRef` — `src/backend/mod.rs` (dormant managed arm).
- `Accelerator{Cpu,Cuda,Rocm,Vulkan,Metal,Npu}` + `AcceleratorSupport` + `Backend::accelerators()`.
- `status.backends` JSON array + N-backend renderer (`status_human`/`status_json`) — `src/cli/output.rs`.
- `BackendChoice{Auto,LlamaCpp}` + `BackendArg{Auto,LlamaCpp}` + `resolve_backend` + `backend_for_identity`.
- TUI picker gating: `backend_choice_available()` (len>2 flips the Backend row on) + Settings R6
  knob-greying scaffold — `src/tui/launch_picker.rs`, `src/tui/tabs/settings.rs`.
- `backend_for_source()` seam (currently always `"llamacpp"`).
- Generic supervisor that keys `ManagedModel` by `ModelId` (used to supervise the umbrella).

## Scope Boundaries

**In scope:**
- Re-add the Lemonade module, typed `lemond` client, fake-responder harness, integration tests.
- Wire dispatch / selection / discovery / routing / eviction / surfacing onto the foundation.
- Manual `lemond` discovery (explicit path or `PATH`) + opt-in enable gate.
- Setup documentation.

**Explicitly OUT (dropped vs original):**
- Auto-install: `src/init/install/lemonade_releases.rs`, the `safe_extract_named`/`LEMOND_BINARY_NAMES`
  generalization, and all `init`-wizard download/extract wiring.
- Installing the NPU **system stack** (XRT, firmware, `flm`) — never in scope; docs point to AMD.

## Requirements Trace

- **R2** — realize the managed-multiplexer lifecycle (live arm). *(re-apply)*
- **R3/R16** — `status` lists backends + accelerators; lemonade row appears. *(re-apply onto foundation renderer)*
- **R6** — Lemonade declares `KnobCapability::none()`; Settings greys unsupported knobs. *(data re-apply; UI scaffold exists)*
- **R9 — CHANGED** — was "fetch/install `lemond`." Now: **find** `lemond` (config path → `PATH`) and
  supervise it; **no download**. Manual install documented.
- **R10** — supervise one `lemond` umbrella; delegate per-model load/unload; inference rides the proxy. *(re-apply)*
- **R11** — Lemonade models from `lemond` `/api/v1/models`, in the catalog tagged by backend. *(re-apply)*
- **R12** — `ModelIdentity` persisted key. *(foundation — done)*
- **R13/R17** — selection: GGUF→llama.cpp (direct); Lemonade-registry→Lemonade; per-model override. *(re-apply)*
- **R14** — TUI list / `list` / `show` backend badge. *(re-apply; list-pane column is the one genuinely-unbuilt piece)*

## Open decision (confirm before U3)

**Supervise vs connect-only.** This plan assumes llamastash **spawns + supervises** `lemond` from
the resolved binary (the original managed-multiplexer design; "provide a flag for us to find
lemond"). The alternative is **connect-only** (user runs `lemond`; we just talk to a running port).
Default = supervise. If you want connect-only, U3/U4 simplify (drop umbrella spawn; discovery/route
just probe the configured port).

## Implementation Units

### Unit 1: Lemonade module + `lemond` client + fake harness
**Goal:** Re-add the engine, self-contained and unit-testable, no wiring yet.
**Files:** `src/backend/lemonade/{mod,client,backend,orchestrate}.rs`, `tests/fixtures/fake_lemond.rs`,
`Cargo.toml` (`[[bin]] fake_lemond`, `required-features=["test-fixtures"]`).
**Source:** side branch verbatim, adjusted to current seam signatures.
- `client.rs`: `GET /live`, `GET /api/v1/health`, `GET /api/v1/models`, `POST /api/v1/load`,
  `POST /api/v1/unload`; typed `LemonadeError`; 120 s load budget.
- `backend.rs`: `impl Backend` as `ManagedMultiplexer`; `capabilities()=KnobCapability::none()`;
  `accelerators()=cpu+npu`; `identify()→ModelIdentity::Backend`.
- `orchestrate.rs`: `umbrella_launch_id()`, ensure-umbrella (idempotent, via generic supervisor,
  readiness=`/live`), delegate load.
**Execution note:** Test-first — re-add the fake-responder contract test per endpoint before each client method.
**Test scenarios:** load posts `{model_name}` → parses `{status:"success"}`; list parses OpenAI shape;
live OK/err; unload; load-timeout maps to a typed error.
**Verification:** `cargo test --features test-fixtures` green; nothing else references the module yet.

### Unit 2: Dispatch + selection
**Goal:** Make Lemonade a first-class enum variant the seam can route to.
**Files:** `src/backend/mod.rs` (`Backends::Lemonade` + all match arms; `backend_for_identity`
`Backend(_)→Lemonade`; `resolve_backend` Lemonade arm), `src/launch/params.rs` (`BackendChoice::Lemonade`),
`src/cli/cli_args.rs` (`BackendArg::Lemonade`, wire `"lemonade"`), `src/cli/output.rs`
(`backend_for_source`: `"lemonade"→"lemonade"`).
**Test scenarios:** `resolve_backend(Backend(_), Auto)→Lemonade`; GGUF identity + `Auto`→LlamaCpp;
explicit `LlamaCpp` override on a Lemonade row still routes llama.cpp; `BackendChoice` serde
round-trips `"lemonade"`.
**Verification:** exhaustive match compiles; selection unit tests green.

### Unit 3: Manual discovery + enable gate (replaces auto-install)
**Goal:** Find a user-provided `lemond`, opt-in, no download.
**Files:** `src/config/loader.rs` + `src/config/mod.rs` (`LemonadeConfig{enabled, binary:Option<PathBuf>, port=13305}`),
`src/cli/cli_args.rs` + `src/cli/daemon.rs` + `src/daemon/mod.rs` (`--lemonade` enable flag;
`LLAMASTASH_LEMONADE=1`; optional `--lemonade-bin` / `--lemonade-port` overrides), `src/discovery/lemonade.rs`
(probe configured port `/api/v1/models`), `src/daemon/discovery_task.rs` (enumerate + watch arm).
**Binary resolution:** `config.lemonade.binary` if set → else `lemond`/`lemonade` on `PATH` → else
"not installed" (surface, don't crash). Enable = `enabled` OR `--lemonade` OR env (any wins).
**Test scenarios:** disabled by default (no discovery, no probe); enabled+binary-on-PATH resolves;
enabled+explicit-path wins; enabled+missing-binary → graceful "not installed".
**Verification:** default build never touches `lemond`; enabling + a (fake) responder lists models.

### Unit 4: Routing + eviction
**Goal:** A Lemonade row launches (ensures umbrella), routes inference, evicts via unload.
**Files:** `src/ipc/methods.rs` (`DelegateToManager` arm: ensure umbrella + `load`; remove the
stub error), `src/proxy/{route,forward,router}.rs` (Lemonade catalog row → umbrella port + `/api`
rewrite; `BackendUnavailable` arm), `src/proxy/eviction.rs` (lifecycle-aware: umbrella models evict
by `unload` API, not SIGTERM), `tests/lemonade_route_test.rs`, `tests/lemonade_umbrella_test.rs`.
**Test scenarios (against fake `lemond`):** start a registry model → umbrella ensured once →
`/v1/chat/completions` forwards to umbrella; second model reuses umbrella; idle eviction calls
`unload` not kill; umbrella down → `BackendUnavailable`.
**Verification:** integration tests green; umbrella spawned exactly once.

### Unit 5: Surfacing (status / badge / picker / list column)
**Goal:** Lemonade visible + controllable across CLI + TUI.
**Files:** `src/ipc/methods.rs` + `src/cli/output.rs` (status `backends` lemonade row:
installed = binary resolvable, accelerators cpu+npu or live), `src/discovery/catalog.rs` (backend tag),
`src/tui/launch_picker.rs` (`BACKEND_CHOICES` += `Lemonade` → flips picker + Settings on),
`src/tui/tabs/settings.rs` (R6 unsupported-knob label already scaffolded), **`src/tui/` list pane**
(backend **badge column** — the genuinely-unbuilt Unit 7 TUI piece).
**Test scenarios:** `status` shows llama.cpp + lemonade rows (installed?/accelerators); `list`/`show`
carry the badge; picker cycles Auto→llama.cpp→Lemonade; greyed knobs render under Lemonade.
**Verification:** `status`/`list`/`show` + TUI surfaces show backends + badge.

### Unit 6: Setup docs
**Goal:** Tell users how to run Lemonade with llamastash.
**Files:** new `docs/lemonade-setup.md`; links from `README.md`, `INSTALL.md`, `FEATURES*`.
**Content:** install Lemonade + sub-backends (llamacpp/ryzenai/flm/whispercpp/sdpp) manually; point
llamastash at it (`lemonade.binary` / `PATH` / `--lemonade*` / env); enable gate; the NPU
system-stack note (we do **not** install XRT/firmware/`flm` — link AMD). No download claims.
**Verification:** doc sweep — no "auto-install"/"init installs lemond" language anywhere.

## Sequencing

U1 → U2 → U3 → U4 → U5. U6 (docs) any time. U1–U2 = engine + seam; U3 = entry; U4 = end-to-end
route; U5 = surfacing. Confirm the **supervise vs connect-only** decision before U3.

## Verification

Unit-testable end-to-end against the re-added `fake_lemond`. Real `lemond` is live on this host at
`:13305` (XDNA2 NPU), so the full path is verifiable here — not just the harness.
