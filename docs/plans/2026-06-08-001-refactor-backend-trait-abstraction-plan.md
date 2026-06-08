---
title: "refactor: Decouple the backend from llama.cpp via a Backend trait (Phase 1)"
type: refactor
status: active
date: 2026-06-08
origin: docs/brainstorms/2026-06-08-multi-backend-abstraction-requirements.md
---

# refactor: Decouple the backend from llama.cpp via a Backend trait (Phase 1)

## Overview

llamastash hardwires `llama-server` into every place it launches, supervises, and
identifies a model. This plan carves a `Backend` seam around those llama.cpp-specific
spots so other inference engines can plug in later — **without changing any
user-visible behavior in this phase.**

The work is phased (per the origin doc):

- **Phase 1 (this plan, implementation-ready):** introduce the `Backend` abstraction
  with **llama.cpp as the sole reference implementation**. Pure refactor — identical
  argv on the wire, identical `/health` probe, identical identity, identical
  CLI/TUI/JSON surfaces. Gated by golden argv-parity tests.
- **Phase 2 (outlined here, research-gated — needs its own `/ce:plan`):** add Lemonade
  (`lemond`) as a second, *managed-multiplexer*-shaped peer backend. Deferred because
  it depends on external research (`lemond`'s API surface, asset naming) the origin
  doc flagged as unresolved.

The guiding constraint from the brainstorm: **the trait must not bake in
"one process per model" or "identity is a local GGUF" assumptions**, even though Phase
1 only ships the process-per-model llama.cpp impl. Phase 1 validates the contract
against Lemonade's *different* shape on paper (a written design walkthrough) before
merge — that paper-validation is what keeps the trait honest with only one impl built.

## Problem Frame

From the origin doc (`docs/brainstorms/2026-06-08-multi-backend-abstraction-requirements.md`):
adding any other inference engine today means editing a dozen load-bearing sites.
Whole capability classes (NPU/XDNA, vLLM, ONNX, whisper, image-gen) sit behind that
coupling. The proxy forward (`src/proxy/forward.rs`) is *already* backend-neutral (a
byte pipe over the OpenAI wire); everything that needs decoupling is on the
**launch / supervise / identify** side.

Grounding the coupling table against the actual code:

| Concern | Site (verified) | Today's assumption |
|---|---|---|
| Binary location | `src/launch/binary.rs::locate` | hardwired `llama-server` |
| Argv composition | `src/launch/params.rs::compose` (line 480) | emits llama-server flags directly |
| Knob vocabulary + capability | `src/launch/flag_aliases.rs` | the typed-knob table *is* llama.cpp's flags |
| Knob defaults | `src/launch/defaults_table.rs::lookup` | `(arch, GpuFlavor) → TypedKnobs` |
| Env strip + spawn | `src/daemon/supervisor.rs::spawn` (lines 343–408) | strips `LLAMA_ARG_*`, one `Command::new(binary)` per model |
| Readiness | `src/daemon/probe.rs::poll_until_ready` | polls llama.cpp `/health` (200 = ready, 503 = loading) |
| Model identity | `src/gguf/identity.rs::compute` | `(canonical path, BLAKE3 of GGUF header)` — assumes a local GGUF |
| Orchestration | `src/ipc/methods.rs::start_model_inner` (line 1074) | calls `compose`/`resolve_layered`/`supervisor_spawn` directly |

**Key structural finding:** most of `src/daemon/supervisor.rs` is *generic process
supervision* (state machine, log rotation, ring buffer, resource sampler, exit
watcher, signal handling). Only `compose()`, the `LLAMA_ARG_*`/`HF_*` env strip, the
`Command::new(binary)` spawn, and the `/health` probe are llama.cpp-specific. **The
trait carves out those four; the generic machinery stays.**

## Requirements Trace

From the origin doc (Phase 1 requirements R1, R2, R4–R8; partial R3/R6 seams; R12
explicitly deferred):

- **R1** — Introduce a `Backend` abstraction owning locate / compose-from-IR / declare
  readiness / sanitise env / define identity. All current llama.cpp behavior moves
  behind it as the reference impl.
- **R2** — The abstraction must express **both lifecycle shapes** (process-per-model
  and managed-multiplexer) without one assuming the other. Phase 1 builds only
  process-per-model but the contract leaves room for the multiplexer (validated on
  paper).
- **R3 (seam only)** — Backends are enumerable (a minimal registry); each declares its
  id and lifecycle shape. Phase 1 registers exactly one. No `backends` listing UI yet.
- **R4** — `TypedKnobs` stays the canonical, backend-neutral IR keyed by llama.cpp's
  vocabulary. Resolver chain (`user > last_used > arch_defaults > builtin`) and Settings
  source-chips unchanged.
- **R5** — Each backend translates resolved IR → its launch config. llama.cpp's
  translation is the existing `compose`/`argvify`, unchanged on the wire.
- **R6 (seam only)** — Each backend declares which IR knobs it supports. Phase 1 exposes
  the capability set (llama.cpp = all knobs) but renders no "unsupported" UI yet.
- **R7** — Phase 1 ships the trait with llama.cpp as sole impl and **zero user-visible
  behavior change**.
- **R8** — Phase 1 is independently mergeable/revertible, gated by golden argv tests
  proving byte-identical llama-server command lines (the
  `LLAMASTASH_BENCH_DISABLE_DEFAULTS` parity contract still holds).

Success criterion this phase must set up (origin doc): *"Adding a third
OpenAI-compatible backend later touches only a new trait impl + a registry entry — no
edits to the supervisor, proxy, resolver, or TUI core."*

## Scope Boundaries

Carried from the origin doc, plus Phase-1-specific cuts:

- **llama.cpp is never routed through any wrapper.** Direct, zero-overhead path is
  non-negotiable.
- **No behavior change.** Phase 1 is behavior-preserving. No new discovery / install /
  recommender / hardware-detection machinery (those are Phase 2+).
- **No `ModelId` schema change.** Identity stays `(path, BLAKE3)` exactly. The R12
  generalisation (enum-ifying `ModelId` for registry-named models) is **deferred to
  Phase 2** — doing the `state.json` migration speculatively now would violate the
  pure-refactor boundary. See Key Technical Decisions for the accepted trade-off.
- **No Lemonade code.** Phase 2 only. Phase 1 includes a *written* Lemonade-mapping
  walkthrough as a design check, not compiled code.
- **No `backends` listing UI, no per-knob "unsupported here" rendering.** Phase 1 lands
  the capability *data* and selection *seam*; the surfacing is Phase 2 (R3/R6 full).
- **No new wire shapes.** Everything still rides the existing OpenAI-compat forward.

## Context & Research

### Relevant Code and Patterns

- **Orchestration seam** — `src/ipc/methods.rs::start_model_inner` (line 1074): the one
  place that resolves knobs (`resolve_layered`, line 1229), auto-fits ctx, picks the
  device-owning binary (lines 1322–1351), and calls `supervisor_spawn(ManagedSpawn{…})`
  (line 1353). This is where backend selection threads in.
- **Argv translation** — `src/launch/params.rs::compose` (line 480) + `argvify`
  (line 211): the llama.cpp IR→argv function. Phase 1's `LlamaCppBackend` delegates to
  these unchanged; parity tests pin their output.
- **Spawn + env strip** — `src/daemon/supervisor.rs::spawn` (line 343): calls
  `compose()` then `Command::new(binary)`, strips `LLAMA_ARG_*`/`HF_*` (lines 373–387),
  stamps `LLAMASTASH_LAUNCHED=1`, spawns via `process_control::platform_default()`. The
  env-strip list and the compose call move into a backend-provided launch spec; the rest
  (ring buffer, rotation, sampler, exit watcher) stays generic.
- **Readiness** — `src/daemon/probe.rs`: `ProbeOptions` + `poll_until_ready` (200=ready,
  503=loading). `scale_for_model` budget logic stays; the *endpoint* (`/health`) becomes
  a backend-declared readiness check.
- **Launch env** — `src/ipc/methods.rs::LaunchEnv` (line 164): holds `binary`,
  `port_range`, `probe`, `arch_defaults`, `device_catalog`. The backend registry hangs
  off here (or off `MethodContext`).
- **Identity** — `src/gguf/identity.rs::compute` + `ModelId`: unchanged in Phase 1; the
  backend's `identify` returns today's `ModelId`.
- **Capability vocabulary** — `src/launch/flag_aliases.rs`: `KnobField`, `KnobSpec`
  table, `knob_row_visible`, `DISPLAY_GROUPS`. The llama.cpp capability set = "every
  `KnobField`." This is where R6's per-backend filter eventually keys off.
- **Defaults** — `src/launch/defaults_table.rs::lookup`: `(arch, GpuFlavor)→TypedKnobs`.
  Stays llama.cpp-flavored in Phase 1 (it produces IR, which is llama.cpp-keyed anyway).

### Existing test patterns to mirror

- **Golden argv** — `src/launch/params.rs` `#[cfg(test)] mod tests` (line 544+):
  `argvify_emits_full_set_in_canonical_order`, `compose_emits_knobs_then_extras_at_tail`,
  `compose_strips_forbidden_extras_flags_and_their_values`. Phase 1 parity tests assert
  `LlamaCppBackend`'s spec argv `==` `compose()` across this matrix.
- **State-machine unit tests** — `src/daemon/supervisor.rs` tests (line 880+): transition
  legality, ring buffer. These must keep passing untouched (proves generic machinery
  intact).
- **Integration guard** — `tests/proxy_autostart.rs` + `tests/fixtures/fake_llama_server.rs`:
  the end-to-end auto-start path. Must pass unchanged (R7 proof at the integration level).
- **Bench parity contract** — `LLAMASTASH_BENCH_DISABLE_DEFAULTS=1`
  (`src/launch/params.rs:420`): when set, defaults are suppressed so the composed argv is
  byte-identical to a hand-run `llama-server`. Phase 1 must preserve this exactly.

### Institutional Learnings

- No `docs/solutions/` directory exists — no recorded prior solutions to fold in.
- AGENTS.md constraints that bind this plan: docs-stay-in-sync; loopback-only security
  (`FORBIDDEN_ADVANCED_PREFIXES` refused in extras, `LLAMA_ARG_*` stripped pre-spawn —
  both must survive the refactor); no pre-release backward-compat obligations; defaults
  table maintenance note.

### External References

- None needed. Internal Rust refactor with strong local patterns (3+ direct examples of
  every pattern the plan touches).

## Key Technical Decisions

- **Dispatch via an `enum Backend`, not `Box<dyn Backend>` + `async_trait`.** The backend
  set is small and closed (llama.cpp now; Lemonade + maybe MLX/FLM later). An enum gives
  zero-cost static dispatch, native `async fn` in the contract (no `async_trait` crate,
  no `dyn`-compatibility gymnastics), and compiler-enforced exhaustiveness so a new
  backend can't silently skip a call site. The behavior contract is still expressed as a
  `Backend` trait the enum forwards to (documents the surface, keeps each impl honest).
  *(See Alternatives Considered for the `dyn` trade-off.)*
- **The trait returns a `LaunchPlan`, not a fixed argv.** The "how do I start a model"
  method yields a `LaunchPlan` whose **process-per-model** variant carries
  `{ binary, argv, env_remove, readiness, probe }` and whose **managed-multiplexer**
  variant (Phase 2) carries an API-delegation request. Phase 1 only constructs the
  process variant, but modelling the result as a plan (not a raw `Vec<OsString>`) is what
  lets Phase 2 add the multiplexer arm *additively* instead of changing the method
  signature. This is the concrete mechanism behind R2.
- **The generic supervisor stays; only the launch *spec* is backend-sourced.**
  `supervisor::spawn` keeps owning the state machine, logs, sampler, exit watcher, and
  signal handling. It stops calling `compose()`/hardcoding the env-strip directly and
  instead consumes the process-per-model `LaunchPlan` the backend produced. Minimises
  diff and blast radius; proves the carve line is correct.
- **`ModelId` is NOT generalised in Phase 1 (accepted trade-off).** The trait's
  `identify` returns today's concrete `ModelId`. The R12 generalisation (making `ModelId`
  an enum so a Lemonade-registry name with no local GGUF can coexist in `state.json` /
  catalog / MRU / failure-tracker keys) *will* touch the `identify` signature in Phase 2.
  We accept that one signature change rather than doing a speculative schema migration
  now — the alternative violates the pure-refactor boundary and risks a `state.json`
  break with no Phase-1 payoff. Flagged loudly so Phase 2 expects it.
- **Capability set is hand-authored per backend.** llama.cpp's set = "all `KnobField`s"
  (derived from `flag_aliases::knob_specs`). No probing. Phase 1 exposes it but renders no
  UI difference (one backend, all-supported). R6's "drop + surface unsupported" rendering
  is Phase 2.
- **llama.cpp `compose`/`argvify` are delegated to, not moved.** `LlamaCppBackend` calls
  the existing `src/launch/params.rs` functions. Keeps the parity tests trivially true
  and avoids a risky code move. (A later cleanup may relocate them under `src/backend/`,
  but not in the parity-critical first cut.)

## Open Questions

### Resolved During Planning

- **Trait vs enum dispatch** → enum dispatch with a documenting `Backend` trait (see Key
  Decisions). Resolvable from repo context (small closed backend set, async methods).
- **Where the registry lives** → on `LaunchEnv` / `MethodContext` (`src/ipc/methods.rs`),
  alongside `binary`/`device_catalog`, since that's already the launch-config home that
  `start_model_inner` reads. Phase 1 registers one backend.
- **What carves into the trait vs stays generic** → trait owns {locate, IR→`LaunchPlan`,
  readiness declaration, env sanitisation, identity, capability set, id, lifecycle
  shape}; supervisor keeps all generic process management. Resolved from reading
  `supervisor.rs` (most of it is engine-agnostic).
- **How to "validate against two shapes" with one impl** → a written Lemonade-mapping
  walkthrough is part of Unit 1's verification (design review), not compiled code.

### Deferred to Implementation

- Exact method names and module split inside `src/backend/` (e.g. whether capability set
  is its own `capability.rs`). Knowable only once the code is in front of us.
- Whether `start_model_inner`'s device→binary selection (lines 1322–1351) reads cleanest
  pushed *into* the backend's `prepare_launch` or stays in the orchestrator and feeds the
  backend the chosen binary. Decide when wiring Unit 3; both preserve behavior.
- Final placement of the `LLAMA_ARG_*`/`HF_*` strip list (in the `LaunchPlan.env_remove`
  vs a backend method). Cosmetic; resolve at Unit 2.

### Deferred to Phase 2 (own `/ce:plan`)

- `lemond` API surface for per-model start/stop/list/health; default port/endpoints;
  autoload-vs-explicit-load; how its lifecycle interacts with the eviction sweeper.
  *(origin doc: "Needs research")*
- `lemond` per-platform asset naming for `pick_asset_suffix`-style routing + install
  footprint. *(origin doc: "Needs research")*
- `ModelId` generalisation + `state.json` coexistence (R12).
- Per-model resource attribution through the multiplexer (R11/R14).
- R6 "unsupported knob" rendering in Settings; R3 `backends` listing surface.

## High-Level Technical Design

> *This illustrates the intended approach and is directional guidance for review, not
> implementation specification. The implementing agent should treat it as context, not
> code to reproduce.*

**The contract (directional pseudo-code):**

```
// src/backend/mod.rs
enum Lifecycle { ProcessPerModel, ManagedMultiplexer }   // R2

enum LaunchPlan {
  // Phase 1 builds only this arm:
  SpawnProcess(ProcessLaunchSpec),
  // Phase 2 adds this arm additively (non-breaking to the SpawnProcess arm):
  // DelegateToManager(ManagerStartRequest),
}

struct ProcessLaunchSpec {
  binary: PathBuf,
  argv: Vec<OsString>,        // from compose(), unchanged on the wire
  env_remove: Vec<&str>,      // the LLAMA_ARG_* / HF_* strip list
  readiness: Readiness,       // e.g. HttpHealth { path: "/health", ready: 200, loading: 503 }
  probe: ProbeOptions,        // scale_for_model already applied by caller
}

trait Backend {
  fn id(&self) -> BackendId;                 // "llamacpp"            (R3)
  fn lifecycle(&self) -> Lifecycle;          // ProcessPerModel       (R2)
  fn capabilities(&self) -> &KnobCapability; // llama.cpp = all knobs (R6)
  fn identify(&self, model: &Path) -> Result<ModelId>;   // Phase 1: gguf::identity::compute
  async fn prepare_launch(&self, r: &ResolvedLaunch) -> Result<LaunchPlan>;  // R5
}

// Dispatch:
enum Backends { LlamaCpp(LlamaCppBackend) /* , Lemonade(...) later */ }
//  forwards each method to the active variant — zero-cost, exhaustive.
```

**Before → after call flow (the actual decoupling):**

```
BEFORE
  start_model_inner ─ resolve_layered ─► LaunchParams
                    └─ supervisor::spawn(ManagedSpawn) ─► compose() + Command::new + env strip + /health probe

AFTER  (behavior identical; only the dotted box is new indirection)
  start_model_inner ─ resolve_layered ─► ResolvedLaunch
                    └─ backend.prepare_launch() ─► LaunchPlan::SpawnProcess(spec)   ┊ NEW SEAM
                    └─ supervisor::spawn(ManagedSpawn { plan: spec, .. })
                         └─ consumes spec.argv / spec.env_remove / spec.readiness
                            (generic state machine, logs, sampler, exit watcher unchanged)
```

The only new indirection is `prepare_launch` producing a `LaunchPlan` that the supervisor
consumes instead of calling `compose()` itself. For llama.cpp the `spec.argv` is exactly
`compose()`'s output — pinned by parity tests.

## Implementation Units

```
Unit 1 (contract) ──► Unit 2 (llama.cpp impl + parity) ──► Unit 3 (wire orchestrator + supervisor) ──► Unit 4 (registry + capability seam)
                                                                                                         (Unit 4 may land with Unit 3)
```

- [x] **Unit 1: Define the `Backend` contract and `LaunchPlan` types (no behavior)**

**Goal:** Land the abstraction's types and trait with zero call-site changes — pure
addition. Establish, via a written Lemonade-mapping walkthrough, that the contract does
not assume process-per-model or local-GGUF identity.

**Requirements:** R1, R2 (contract shape), R3/R6 (type seams only).

**Dependencies:** None.

**Files:**
- Create: `src/backend/mod.rs` (the `Backend` trait, `Backends` dispatch enum scaffold,
  `Lifecycle`, `LaunchPlan` with the `SpawnProcess` arm, `ProcessLaunchSpec`,
  `Readiness`, `BackendId`, `KnobCapability`).
- Modify: `src/lib.rs` (register `pub mod backend;`).
- Test: `src/backend/mod.rs` (`#[cfg(test)]` for type-level invariants).

**Approach:**
- Types + trait only; no impl wired anywhere yet. Compiles as dead-but-valid code.
- `LaunchPlan` is an enum with one variant now; documented as additively extensible for
  the Phase 2 multiplexer arm (so adding it later doesn't change `prepare_launch`'s
  signature). Mark intent in a doc-comment rather than `#[non_exhaustive]` unless an
  external crate consumes it (it doesn't).
- `KnobCapability` keys off `flag_aliases::KnobField` so "supported set" is expressed in
  the existing vocabulary.

**Technical design:** see High-Level Technical Design pseudo-code above (directional).

**Patterns to follow:**
- Module-doc + type-doc density of `src/launch/params.rs` and `src/daemon/probe.rs`.
- `flag_aliases::KnobField` for the capability key type.

**Test scenarios:**
- Happy path: `LlamaCppBackend`-independent type construction — a `ProcessLaunchSpec` can
  be built and its fields read back (proves the shape is usable).
- Edge case: `KnobCapability` "all supported" vs "subset" — `supports(field)` returns the
  expected boolean for an all-knobs set and a hand-built subset.
- Design check (not a code test, gates the unit): a written walkthrough in the module doc
  mapping each `Backend` method to *how Lemonade would implement it* (multiplexer
  lifecycle, registry identity, API-delegation `LaunchPlan` arm). The unit is not "done"
  until this walkthrough shows no method forces a process-per-model or local-GGUF
  assumption.

**Verification:**
- `cargo build` clean; new module present and exported; no existing call site changed;
  full existing test suite still green (nothing wired yet).

---

- [x] **Unit 2: Implement `LlamaCppBackend` as the reference impl + golden parity tests**

**Goal:** Implement the trait for llama.cpp by delegating to existing launch logic, and
prove the produced launch spec is byte-identical to today's `compose()` output.

**Requirements:** R1, R4, R5, R8 (parity).

**Dependencies:** Unit 1.

**Files:**
- Create: `src/backend/llama_cpp.rs` (`LlamaCppBackend` implementing `Backend`).
- Modify: `src/backend/mod.rs` (`Backends::LlamaCpp` variant + forwarding).
- Test: `src/backend/llama_cpp.rs` (`#[cfg(test)]` golden parity tests).

**Approach:**
- `prepare_launch` → builds `ProcessLaunchSpec { binary, argv: compose(&params, port),
  env_remove: <the existing strip list>, readiness: HttpHealth{/health,200,503},
  probe }`. **Delegates to `src/launch/params.rs::compose`** — does not reimplement argv.
- `identify` → `crate::gguf::identity::compute` (unchanged).
- `capabilities` → all `KnobField`s (from `flag_aliases::knob_specs`).
- `id` → `"llamacpp"`; `lifecycle` → `ProcessPerModel`.
- The `LLAMA_ARG_*`/`HF_*` strip list moves from `supervisor::spawn` into the spec's
  `env_remove` (or a backend method) so the loopback/credential contract is now
  backend-declared but identical in content.

**Execution note:** Write the parity tests first (they encode the R8 contract), then make
`prepare_launch` satisfy them.

**Patterns to follow:**
- `src/launch/params.rs` test module (line 544+): `argvify_emits_full_set_in_canonical_order`,
  `compose_emits_knobs_then_extras_at_tail`, the forbidden-extras-strip tests.

**Test scenarios:**
- Happy path: for a representative `LaunchParams` (model path + ctx + a multi-GPU knob
  set + extras), `prepare_launch(...).argv == compose(&params, port)` exactly.
- Happy path: `env_remove` contains exactly the set `supervisor::spawn` strips today
  (`LLAMA_ARG_HOST/PORT/BIND/LISTEN/API_KEY/SSL_*`, `HF_TOKEN`, `HUGGING_FACE_HUB_TOKEN`,
  `HF_HOME`, `HF_ENDPOINT`) — no additions, no omissions.
- Edge case: empty knobs + chat mode → argv is the minimal `--host 127.0.0.1 --port N
  -m <path>` set (matches `compose` with defaults).
- Edge case: embedding and rerank modes emit `--embeddings` / `--reranking` respectively
  (parity with `compose`'s mode arm).
- Error/contract path: extras containing a forbidden flag (`--host 0.0.0.0`) are stripped
  in the spec's argv exactly as `compose` strips them (loopback contract preserved).
- Parity-contract: with `LLAMASTASH_BENCH_DISABLE_DEFAULTS=1`, the spec argv is identical
  to the pre-refactor `compose` output for the same inputs (defaults suppressed).
- `identify`: returns a `ModelId` equal to `gguf::identity::compute(path, header)` for the
  same file (delegation proven, not reimplemented).
- `capabilities`: every `KnobField` reports supported.

**Verification:**
- All parity tests green; `cargo test` whole-suite green; the impl contains no argv
  string literals that duplicate `compose` (it delegates).

---

- [x] **Unit 3: Route `start_model_inner` + `supervisor::spawn` through the backend**

**Goal:** Replace the direct `compose()` + hardcoded-env-strip calls with the backend's
`LaunchPlan`. This is the actual decoupling. Behavior must stay identical.

**Requirements:** R1, R5, R7 (no behavior change).

**Dependencies:** Unit 2.

**Files:**
- Modify: `src/ipc/methods.rs::start_model_inner` (line 1074) — obtain the backend, call
  `prepare_launch`, pass the resulting process spec into `ManagedSpawn`.
- Modify: `src/daemon/supervisor.rs::spawn` (line 343) — consume `spec.argv` /
  `spec.env_remove` / `spec.readiness` instead of calling `compose()` and hardcoding the
  strip list; keep all generic machinery (state machine, logs, sampler, exit watcher,
  signal handling) untouched.
- Modify: `src/daemon/supervisor.rs::ManagedSpawn` — carry the process launch spec (or
  the resolved plan) instead of (or alongside) raw `params` + `binary`.
- Modify: `src/daemon/probe.rs` usage — probe endpoint comes from `spec.readiness` rather
  than being hardcoded (the `poll_until_ready` body can stay; the `/health` path becomes
  a parameter sourced from the readiness declaration).
- Test: `tests/proxy_autostart.rs` (existing — must pass unchanged); `src/daemon/supervisor.rs`
  tests (existing — must pass unchanged).

**Approach:**
- `start_model_inner` keeps doing port allocation, `resolve_layered`, ctx auto-fit, and
  device→binary selection (lines 1132–1351) — those are orchestration, not engine
  translation. It then asks the backend for the `LaunchPlan` and threads the
  `SpawnProcess` spec into `ManagedSpawn`.
- The env-strip loop in `supervisor::spawn` (lines 373–387) is driven by
  `spec.env_remove`; `LLAMASTASH_LAUNCHED=1` stamp and `process_control` spawn stay.
- Decision deferred to this unit (both behavior-preserving): keep device→binary selection
  in the orchestrator and feed the backend the chosen binary, **or** push it into
  `prepare_launch`. Prefer the former for a smaller diff unless the latter reads cleaner.

**Execution note:** This is the regression-risk unit. Lean on the existing integration
test (`tests/proxy_autostart.rs`) and supervisor unit tests as the behavior-preserving
guard; do not modify them to make the refactor pass.

**Patterns to follow:**
- `ManagedSpawn` construction at `src/ipc/methods.rs:1353` (the current call shape).
- The env-strip rationale comment block at `src/daemon/supervisor.rs:355–400` (preserve
  the security intent verbatim; only its *source* moves).

**Test scenarios:**
- Happy path (integration): `tests/proxy_autostart.rs` passes unchanged — auto-start
  composes the same argv, probes `/health`, reaches Ready, forwards.
- Happy path: a manual `start_model` for a chat model spawns with the same argv it did
  pre-refactor (assert against `compose` output via the supervisor's recorded params, or
  a fake-binary argv capture in `tests/fixtures/fake_llama_server.rs`).
- Edge case: device selector set → the correct owning binary is still chosen and
  `--device` still emitted (device→binary path intact; lines 1322–1351 behavior preserved).
- Edge case: embedding-mode auto-start still emits `--embeddings` (the mode_hint path in
  `src/proxy/launch.rs` is unaffected).
- Error path: forbidden extras flag → still rejected with the same `InvalidParams` error
  and the reserved port is released (the `forbidden_in_extras` guard at line 1293 still
  fires before spawn).
- Error path: spawn failure → reserved port released, `InternalError` surfaced (unchanged
  from line 1364).
- Integration: env strip still removes `LLAMA_ARG_*`/`HF_*` before spawn — assert via a
  fake binary that echoes its environment (extend `fake_llama_server.rs` if needed) that
  none of the stripped vars survive.
- State machine: supervisor transitions `Launching→Loading→Ready` and the terminal-state
  guards still behave (existing supervisor tests stay green).

**Verification:**
- Whole suite green with **no edits to the assertions** in `tests/proxy_autostart.rs` or
  the supervisor tests. `start_model_inner` no longer calls `compose()` directly; the only
  `compose()` caller is `LlamaCppBackend`. Manual smoke: `llamastash start <model>`
  produces an identical `llama-server` command line (verify via the per-launch log).

---

- [x] **Unit 4: Minimal backend registry + capability seam (enumerable, one backend)**

**Goal:** Make backends enumerable and make backend selection explicit (always llama.cpp
in Phase 1), and expose the capability set to the knob-resolution path — without any
user-visible change. Sets up R3/R6 for Phase 2.

**Requirements:** R3 (seam), R6 (seam).

**Dependencies:** Unit 3. *(May land together with Unit 3 if the registry is the natural
way to obtain the backend there.)*

**Files:**
- Modify: `src/backend/mod.rs` — a `registry()`-style function returning the available
  `Backends` (just llama.cpp), and a `select_backend(model)`-style helper that returns
  llama.cpp for every GGUF (the automatic-selection rule from R13, trivial in Phase 1).
- Modify: `src/ipc/methods.rs` (`LaunchEnv` or `MethodContext`) — hold the registry so
  `start_model_inner` selects through it rather than assuming llama.cpp.
- Test: `src/backend/mod.rs` (`#[cfg(test)]`).

**Approach:**
- Registry is a tiny enumeration (Phase 1: one entry). `select_backend` returns
  `Backends::LlamaCpp` for any local GGUF — the deterministic-from-source rule the
  brainstorm specifies (R13), so no user choice is introduced.
- Capability set is reachable from where the Settings editor will eventually gate knob
  rows (`flag_aliases::knob_row_visible`), but Phase 1 wires only the data path — the
  function still returns the same visibility for the single backend, so the TUI is
  byte-identical.

**Test scenarios:**
- Happy path: `registry()` lists exactly one backend with id `"llamacpp"` and lifecycle
  `ProcessPerModel`.
- Happy path: `select_backend(<any .gguf path>)` returns the llama.cpp backend.
- Edge case: capability lookup for llama.cpp reports all `KnobField`s supported (so any
  backend-aware gating is a no-op in Phase 1).
- Integration: `start_model_inner` obtains its backend via the registry (not a hardcoded
  constructor) — assert the selection function is on the call path (e.g. via a test that
  swaps a stub backend into the registry and observes it used). *If a swap seam is more
  machinery than Phase 1 warrants, downgrade this to a direct unit test of
  `select_backend` and note the indirection is exercised by Unit 3's integration test.*

**Verification:**
- Suite green; TUI Settings and `llamastash list`/`status` output unchanged (no backend
  badge yet — that's Phase 2). The selection seam exists and is the only way
  `start_model_inner` reaches a backend.

## Phased Delivery

### Phase 1 (this plan)
Units 1–4. Ships the `Backend` seam with llama.cpp as the sole, byte-identical reference
impl. Independently mergeable and revertible. Done when: parity tests green, integration
tests unchanged-and-green, manual smoke shows identical `llama-server` command lines, and
the Lemonade-mapping design walkthrough (Unit 1) confirms no shape leakage.

### Phase 2 (separate `/ce:plan`, research-gated)
**Do not start until** the `lemond` research gates close (API surface, asset naming —
origin doc "Needs research"). Rough shape, not implementation-ready:
- Add `Backends::Lemonade` with `Lifecycle::ManagedMultiplexer` and the
  `LaunchPlan::DelegateToManager` arm (additive to Unit 1's enum).
- Supervise one long-lived `lemond` via the **generic** process supervisor; delegate
  per-model start/stop/list to its API.
- Fetch/install `lemond` through the existing prebuilt-asset path (`src/init/fetch.rs` /
  `download.rs`), opt-in.
- Generalise `ModelId` (R12) + `state.json` coexistence — this touches the `identify`
  signature (accepted in Phase 1's Key Decisions).
- Land R3 `backends` listing + R6 "unsupported knob" Settings rendering + R14 backend
  badge.

## System-Wide Impact

- **Interaction graph:** the new seam sits between `start_model_inner`
  (`src/ipc/methods.rs`) and `supervisor::spawn` (`src/daemon/supervisor.rs`). The proxy
  auto-start path (`src/proxy/launch.rs`) calls `start_model_inner` and is therefore
  affected transitively — covered by `tests/proxy_autostart.rs`.
- **Error propagation:** `prepare_launch` can fail (e.g. identity read). It must surface
  as the same `InternalError`/`InvalidParams` shapes `start_model_inner` already returns,
  and must release any reserved port on failure (mirror the existing line 1364 handling).
- **State lifecycle risks:** none new — the supervisor state machine, port reservation
  CAS, and failure-tracker are untouched. The risk is *accidental* behavior drift in the
  refactor, which the parity + integration tests guard.
- **API surface parity:** IPC `start_model`, CLI `start`, TUI Launch, and proxy
  auto-start all funnel through `start_model_inner` — routing it through the backend covers
  every entry point at once (no per-surface change needed). This is the leverage point.
- **Integration coverage:** the fake-binary fixture (`tests/fixtures/fake_llama_server.rs`,
  already modified in the working tree) is the right place to assert argv + stripped-env
  end to end.
- **Unchanged invariants (blast-radius assurance):** the loopback-only contract
  (`FORBIDDEN_ADVANCED_PREFIXES` strip + `LLAMA_ARG_*` removal), the
  `LLAMASTASH_LAUNCHED=1` stamp, `ModelId`'s `(path, BLAKE3)` shape and `state.json`
  serialisation, the `/health` 200/503 semantics, and all CLI/TUI/JSON output remain
  exactly as today. Phase 1 changes *where* these live, never *what* they do.

## Risks & Dependencies

| Risk | Mitigation |
|------|------------|
| Refactor silently changes argv (breaks the zero-overhead promise + benchmarks) | Golden parity tests assert spec argv `== compose()` byte-for-byte across a knob matrix incl. `LLAMASTASH_BENCH_DISABLE_DEFAULTS=1`; `LlamaCppBackend` delegates to `compose` rather than reimplementing. |
| Trait designed only against llama.cpp grows process-per-model / local-GGUF assumptions, making Phase 2 a rewrite | `LaunchPlan` enum models both lifecycle shapes from day one; Unit 1's written Lemonade-mapping walkthrough is a merge gate; the `identify`-signature change is pre-acknowledged, not a surprise. |
| Env-strip / loopback contract weakened by moving the strip list | Unit 2 test asserts `env_remove` is exactly today's set; Unit 3 integration test asserts stripped vars don't survive to the child. |
| Over-engineering the registry beyond Phase 1's behavior-preserving boundary | Registry is one entry + a trivial `select_backend`; no listing UI, no install machinery (explicit scope cut). |
| Device→binary selection (multi-GPU) regresses during the move | Unit 3 keeps the existing selection logic in the orchestrator by default; edge-case test asserts `--device` + owning-binary still chosen. |
| `ModelId` signature churn in Phase 2 ripples wider than expected | Accepted and documented now; isolated to `identify` + the Phase 2 schema migration, which gets its own plan. |

## Documentation / Operational Notes

- Update `src/backend/mod.rs` module doc to be the canonical description of the seam
  (per AGENTS.md docs-stay-in-sync). No user-facing docs change in Phase 1 (no behavior
  change). The origin brainstorm already records the strategy.
- No rollout/migration/monitoring concerns — internal refactor, no schema or wire change.
- TODO.md already tracks this item (linked to the brainstorm); flip it to reference this
  plan when work starts.

## Sources & References

- **Origin document:** [docs/brainstorms/2026-06-08-multi-backend-abstraction-requirements.md](../brainstorms/2026-06-08-multi-backend-abstraction-requirements.md)
- Prior art (superseded for "how backends plug in"): [docs/brainstorms/2026-05-31-npu-backend-via-lemonade-requirements.md](../brainstorms/2026-05-31-npu-backend-via-lemonade-requirements.md)
- Key code seams: `src/ipc/methods.rs::start_model_inner` (line 1074),
  `src/launch/params.rs::compose` (line 480), `src/daemon/supervisor.rs::spawn`
  (line 343), `src/daemon/probe.rs`, `src/gguf/identity.rs`, `src/launch/flag_aliases.rs`,
  `src/launch/defaults_table.rs`.
- Parity contract: `LLAMASTASH_BENCH_DISABLE_DEFAULTS` (`src/launch/params.rs:420`).
- Behavior guards: `tests/proxy_autostart.rs`, `tests/fixtures/fake_llama_server.rs`.

## Alternative Approaches Considered

- **`Box<dyn Backend>` + `async_trait`.** Rejected for Phase 1: adds the `async_trait`
  crate, dynamic dispatch, and `dyn`-compatibility constraints for a backend set that is
  small and closed. Enum dispatch gives zero-cost static dispatch, native `async fn`, and
  compiler-enforced exhaustiveness. If the backend set ever becomes plugin-loaded at
  runtime, revisit — but that's not on the roadmap.
- **Move `compose`/`argvify` into `src/backend/llama_cpp.rs` in Phase 1.** Rejected for
  the first cut: relocating the parity-critical function adds risk for no behavior
  benefit. Delegate now; a cosmetic relocation can follow once the seam is proven.
- **Generalise `ModelId` now (Phase 1).** Rejected: it's a `state.json` schema migration
  with no Phase-1 payoff and violates the pure-refactor boundary. Deferred to Phase 2
  with the signature change pre-acknowledged.
- **Plan Phase 2 (Lemonade) in this document.** Rejected: it depends on external research
  the origin doc flagged unresolved (`lemond` API + asset naming). Planning it now would
  manufacture false certainty; it gets its own `/ce:plan` once research closes.
