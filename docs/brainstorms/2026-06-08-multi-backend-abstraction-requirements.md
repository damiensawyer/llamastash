---
date: 2026-06-08
topic: multi-backend-abstraction
---

# Multi-Backend Support: Decouple the Backend from llama.cpp

> Origin: brainstorm on 2026-06-08 — "make the backend uncoupled from llama.cpp;
> we should be able to add other backends and translate the args from the
> llama.cpp ones to whatever other backends need." Supersedes the framing in
> [`2026-05-31-npu-backend-via-lemonade-requirements.md`](2026-05-31-npu-backend-via-lemonade-requirements.md)
> for *how* backends plug in (that doc's Tier-1/2 NPU analysis still holds for the
> Lemonade specifics). This doc defines the abstraction; planning turns it into units.

## Problem Frame

llamastash is hardwired to `llama-server` everywhere it launches, supervises, and
identifies a model. Adding any other inference engine today means editing a dozen
load-bearing sites. Yet whole classes of capability sit just out of reach behind
that coupling: NPU inference (AMD XDNA), vLLM, ONNX/OGA, whisper ASR, image
generation — none of which llama.cpp does.

The one piece that is *already* backend-neutral is the proxy forward
(`src/proxy/forward.rs`) — a byte pipe over the OpenAI wire with no llama-server
assumptions. Everything that needs decoupling is on the **launch / supervise /
identify** side:

| Concern | Site | Today's assumption |
|---|---|---|
| Binary location | `src/launch/binary.rs` | hardwired `llama-server` |
| Argv composition | `src/launch/params.rs::compose` | emits llama-server flags directly |
| Knob vocabulary | `src/launch/flag_aliases.rs` | the typed-knob table *is* llama.cpp's flags |
| Knob defaults | `src/launch/defaults_table.rs` | `(arch, gpu_backend) → TypedKnobs` |
| Spawn + env strip | `src/daemon/supervisor.rs::spawn` | strips `LLAMA_ARG_*`, one process per model |
| Readiness | `src/daemon/probe.rs` | polls llama.cpp `/health` |
| Model identity | `src/gguf/` | `(canonical path, BLAKE3 of GGUF header)` — assumes a local GGUF file |
| Discovery | `src/discovery/` | scans disk for `.gguf` |

`TypedKnobs` is **already a backend-neutral intermediate representation** — it just
uses llama.cpp's flag names. So "translate the args" is really: keep `TypedKnobs`
as the canonical IR, make each backend own `IR → its argv/config`, and make
llama.cpp the reference implementation of a `Backend` trait rather than the only
thing that exists.

**Strategic decision (resolved):** llama.cpp stays a **direct, first-class,
zero-overhead** backend — never routed through any wrapper. **Lemonade** joins as
**one peer backend** behind the trait: the single door to NPU + vLLM + ONNX +
whisper + image-gen, letting `lemond` fan out to those engines (llamastash adds no
transparency or knob-tuning value translating ONNX/NPU itself). This preserves the
product's moat — transparent, auto-tuned llama.cpp — while getting broad engine
reach from one integration.

## The two backend lifecycle shapes

Designing the trait against Lemonade surfaces the key constraint: **the trait must
not assume one process per model.**

```
Process-per-model  (llama.cpp — Phase 1)
  llamastash ── spawn 1 llama-server per model ──► /health probe ──► evict = kill it
  llamastash owns the FULL lifecycle.

Managed-multiplexer  (Lemonade — Phase 2)
  llamastash ── supervise 1 long-lived `lemond` ──► its /api/v1/models
                                                 └─ per-model start/stop/list
                                                    delegated to lemond's API
  llamastash owns the UMBRELLA process; the backend owns per-model lifecycle.
  lemond itself fans out to llamacpp / ryzenai / flm / whispercpp / sdpp.
```

## Requirements

**Backend abstraction (the seam)**
- R1. Introduce a `Backend` trait (or equivalent) that owns everything currently
  hardwired to llama.cpp: locating/launching the engine, composing its launch
  config from the neutral IR, declaring readiness, sanitising the environment,
  and defining model identity/lifecycle. All current llama.cpp behavior moves
  behind it as the reference implementation.
- R2. The trait must express **both lifecycle shapes** (process-per-model and
  managed-multiplexer) without one assuming the other. The supervisor, proxy
  auto-start, and eviction sweeper interact with backends through the trait, not
  through llama.cpp-specific calls.
- R3. Backends are enumerable at runtime (a registry), each declaring its name,
  the knobs it supports, where its models come from, and its lifecycle shape — the
  data a `backends`-style listing and backend-aware UI need. (Mirrors Lemonade's
  `lemonade backends`.)

**Knob translation (the IR)**
- R4. `TypedKnobs` remains the canonical, backend-neutral knob IR, keyed by
  llama.cpp's vocabulary (the lingua franca users already know). The resolver
  chain (`user > last_used > arch_defaults > builtin > model_default`) and the
  Settings source-chips stay unchanged and backend-agnostic.
- R5. Each backend translates the resolved IR into its own launch config. llama.cpp
  translation is the existing `compose`/`argvify` path, unchanged on the wire.
- R6. Each backend declares which IR knobs it supports. A set knob a backend can't
  honor is **dropped from that backend's launch, logged, and surfaced in the
  Settings editor** as greyed / "not supported by `<backend>`" — never silently
  ignored, never a hard launch block. (Reuses the existing `knob_row_visible`
  gating, made backend-aware.)

**Phase 1 — llama.cpp behind the trait (pure refactor)**
- R7. Phase 1 ships the trait with llama.cpp as the sole implementation and **zero
  user-visible behavior change**: identical argv on the wire, identical `/health`
  probe, identical identity, identical CLI/TUI/JSON surfaces.
- R8. Phase 1 is independently mergeable and revertible, gated by golden argv tests
  proving the composed llama-server command line is byte-identical to today's for
  the same inputs (the bench `LLAMASTASH_BENCH_DISABLE_DEFAULTS` parity contract
  must still hold).

**Phase 2 — Lemonade peer backend**
- R9. Add Lemonade as a second backend using the embeddable `lemond` binary —
  fetched, installed, and supervised through the same install/asset-routing path
  llamastash already uses for llama.cpp prebuilts (`lemond` ships as per-platform
  `.zip`/`.tar.gz`, the same shape). Opt-in: `lemond` is only fetched when the user
  enables the Lemonade backend.
- R10. llamastash supervises the single `lemond` process; per-model start / stop /
  list on that backend are delegated to `lemond`'s API. Inference flows through the
  existing OpenAI-compat proxy forward, unchanged.
- R11. Lemonade-backed models are sourced from `lemond`'s model endpoint
  (`/api/v1/models`) and appear in the catalog tagged with their backend.

**Model identity, discovery & selection**
- R12. Model identity generalises beyond "local GGUF path + header BLAKE3" enough to
  name a Lemonade-registry model that has no local GGUF, without breaking the
  existing GGUF identity for llama.cpp models.
- R13. The catalog carries a per-row `backend` tag. GGUF models discovered on disk
  (and Ollama/LM Studio caches) always bind to the **direct llama.cpp** backend —
  never routed through Lemonade even though `lemond` could also run GGUF. Backend
  selection is automatic from the model's source/format; no user choice required
  in the common case.
- R14. The TUI list, `llamastash list`, and `show`/`status` surface which backend a
  model runs on (e.g. a backend badge), so the two model namespaces (GGUF library
  vs Lemonade registry) read as one coherent catalog.

## Success Criteria
- Adding a *third* OpenAI-compatible backend later touches only a new trait impl +
  a registry entry — no edits to the supervisor, proxy, resolver, or TUI core.
- After Phase 1, the llama.cpp path is byte-identical on the wire and shows no
  benchmark regression vs the pre-refactor binary.
- After Phase 2, a Strix-Halo user can run NPU inference end-to-end through
  llamastash (the original motivating ask), and the llama.cpp experience is
  completely unchanged for users who never enable Lemonade.
- A user who sets a knob the active backend can't honor understands why it had no
  effect (it's visibly marked unsupported), rather than filing it as a bug.

## Scope Boundaries
- **llama.cpp is never routed through Lemonade or any wrapper.** The direct,
  zero-overhead path is non-negotiable; that's the product's reason to exist.
- Phase 1 ships **no** new discovery/install/recommender machinery — it is a
  behavior-preserving refactor only. All multi-backend surfacing lands in Phase 2+.
- No backend-aware **recommender** in this scope. `init`'s model recommendation
  stays llama.cpp/GGUF-only; Lemonade models are listed, not recommended. (Deferred.)
- No NPU **hardware detection** (`/dev/accel`, amdxdna) in this scope — enabling the
  Lemonade backend is user-driven, not auto-detected. (Deferred; see prior NPU doc Tier 3.)
- No new inference **wire shapes**. Everything rides the existing OpenAI-compat
  forward. Anthropic `/v1/messages`, Ollama-native inference, and Responses-API
  shims remain their own separate TODO items.
- No in-process engines. The abstraction is for **supervised, OpenAI-compatible
  subprocesses** (which is what llama.cpp, `lemond`, vLLM, MLX server all are).
- Not adopting Lemonade's `recipe_options.json` / `server_models.json` as
  llamastash's own config — those stay `lemond`'s internal concern behind its API.

## Key Decisions
- **Peer backends, not umbrella.** Fronting `lemond` as a universal multiplexer that
  llama.cpp also flows through was rejected: it re-hides llama.cpp behind a heavy
  abstraction (the thing llamastash positions against), voids "zero overhead vs raw
  llama-server," strands the knob-tuning IP, nests two supervisors (breaking
  per-model resource attribution + eviction), and forces two mismatched model
  namespaces together. Lemonade-for-the-exotic-long-tail keeps the moat and still
  gets broad reach from one integration.
- **Trait first, Lemonade second.** Smaller, independently reviewable diffs;
  regressions on the core path are bisectable; matches the incremental workflow.
- **llama.cpp vocabulary is the IR.** Users already know `-ngl`, `--n-cpu-moe`,
  `--tensor-split`; backends translate *from* that rather than inventing a new
  neutral dialect. Matches the literal request ("translate the args from the
  llama.cpp ones").
- **Drop + surface unsupported knobs.** Honest and discoverable; avoids both the
  "looks like a bug" silent-drop and the "trips on every inherited knob" hard-block.

## Future direct backends (MLX, FLM, vLLM)

The trait is designed so any **supervised, OpenAI-compatible subprocess** becomes a
peer with just a new impl + a registry entry (Success Criteria). Two concrete future
backends show the design holding — and where to be careful:

- **MLX** (Apple Silicon, `mlx_lm.server`) is a clean **direct** peer: same
  process-per-model lifecycle as llama.cpp (R2 shape 1). Work is mostly knob
  translation — MLX has almost no launch knobs (unified memory ⇒ no `-ngl`, quant
  baked into the model), so most IR knobs **drop + surface as unsupported** (R6).
- **FLM** (FastFlowLM, AMD XDNA NPU) is reachable **two ways, and the trait supports
  both**: (1) *free, via Lemonade* — `lemond` already fans out to `flm` (see lifecycle
  diagram), so it ships with Phase 2 at no extra integration cost; (2) *direct peer
  later* — `flm serve` is its own OpenAI-compatible process-per-model server, so it can
  be promoted to a tuned direct backend (zero Lemonade overhead, per-model attribution)
  exactly like MLX, **if it earns the investment**. Same start-cheap-then-promote path
  applies to **vLLM**.

This via-Lemonade-vs-direct choice is per backend and deferred — not a structural
limit. Two caveats to design deliberately, not discover late:

- **Backend-unique knobs have no IR slot.** The IR is keyed to llama.cpp's vocabulary
  (R4), so a knob with *no* llama.cpp equivalent (e.g. an MLX LoRA-adapter path) can't
  be expressed as a typed knob. The escape hatch is the existing free-form `extras`
  passthrough — adequate, but it should be a conscious decision, not an accident.
- **Validate the trait against two shapes before Phase 1 is "done."** If the trait only
  ever sees llama.cpp it will quietly grow llama.cpp-shaped assumptions and a third
  backend won't be drop-in. Proving it against Lemonade's *multiplexer* shape (R2 shape
  2) is what makes MLX / FLM-direct the easy adds the Success Criteria promise.

## Dependencies / Assumptions
- The proxy forward (`src/proxy/forward.rs`) is and stays format-agnostic — the
  load-bearing assumption that makes any OpenAI-compat backend cheap. (Verified.)
- `lemond` (embeddable Lemonade) ships as a portable per-platform binary archive
  and exposes an OpenAI-compatible API incl. a models endpoint. (Verified against
  Lemonade's embeddable docs; exact port/endpoint paths to confirm in planning.)
- Lemonade platform reach (Windows x64 / Ubuntu x64 / macOS ARM64) is acceptable
  for an opt-in backend.

## Outstanding Questions

### Resolve Before Planning
- _(none — strategy, phasing, knob-mismatch policy, and llama.cpp-direct rule are
  all decided above.)_

### Deferred to Planning
- [Affects R1/R2][Technical] Exact trait shape and where the registry lives — async
  trait vs enum-dispatch, and how the supervisor/proxy/eviction call sites are
  threaded through it without churning the llama.cpp golden paths.
- [Affects R12][Technical] Generalised `ModelId` representation — how a
  Lemonade-registry name coexists with GGUF `(path, BLAKE3)` in `state.json`,
  catalog rows, MRU, and failure-tracker keys without a schema break on the GGUF side.
- [Affects R10][Needs research] `lemond`'s exact API surface for per-model
  start/stop/list/health and its default port/endpoints; whether it autoloads on
  first request or needs an explicit load call; how its idle/lifecycle interacts
  with llamastash's eviction sweeper.
- [Affects R9][Needs research] `lemond` asset naming per platform/arch for
  `pick_asset_suffix`-style routing, and its install footprint (size, any runtime
  prereqs like drivers/EPs the binary doesn't bundle).
- [Affects R11/R14][Technical] How per-model resource attribution (host pane
  RSS/CPU%/VRAM) degrades for Lemonade-managed models, and what to show instead
  (e.g. attribute to the `lemond` umbrella, or query its API if it exposes per-model stats).
- [Affects R6][Technical] Source of each backend's knob-capability set — hand-authored
  per backend vs probed — and how the Settings editor renders "unsupported here."

## Next Steps
-> `/ce:plan` for structured implementation planning (start with Phase 1: the trait
   + llama.cpp reference impl behind golden-argv parity tests).
