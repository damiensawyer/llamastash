# TODO

Single index of outstanding work across plans, docs, and code. When you add a
TODO anywhere in the repo (a `TODO(...)` comment, an unchecked `- [ ]` in a
plan, a `todo:` frontmatter field on a spike), also add a one-line entry here
with a link back to the source. When you complete one, strike it from both
places.

Two release tracks:

- **R1 (v0.0.1)** — first public release. Bar: software is usable for its
  core purpose (init → daemon → TUI), distributed via the release pipeline,
  with docs and audit clean. Bug fixes and small UX polish only.
- **R2 (post-v0.0.1)** — everything queued behind R1: feature work, platform
  expansion, recommendation-quality parity, and longer-horizon brainstorms.

## In-code TODOs

_None — the four vendoring items shipped 2026-05-19 via [`docs/plans/2026-05-19-001-feat-vendor-benchmark-scrapers-plan.md`](docs/plans/2026-05-19-001-feat-vendor-benchmark-scrapers-plan.md). The Open LLM Leaderboard + Aider polyglot adapters now run live against upstream in the daily CI cron at the pinned whichllm commit `73cd92f`; both `TODO(unit7-v2-ga)` placeholders in `scripts/regenerate-benchmark-snapshot.py` are gone._

## R1 (v0.0.1) — first release

### Blockers

- [x] ~~Init does not hand off to TUI after all steps.~~ — `init` now prompts to launch the TUI on success (auto-launch with `--recommended`, skip with `--no-tui`).
- [ ] Add copy feature for logs pane. When in log pane, c should copy the full log text to clipboard and show a visual confirmation (e.g. a toast or temporary message in the UI).
- [ ] copy actions(url,path,curl,logs) should show a visual confirmation (e.g. a toast or temporary message in the UI).
- [x] ~~The UI here in `init` doesn't look nice. Make those info inline with remaining UI.~~ — summary now renders via `cliclack::note` so every line keeps the panel border, then a single-line `outro` closes the session.

- [ ] **In progress**: Proxy router that maps a single endpoint to running models by model name. If the model isn't running, start it; if launch fails, fall back to a running model when one is available; otherwise error. Keep it OpenCode / π compatible so agents and tools can hit one URL.

### Release checklist

- [ ] **In progress**: Benchmark against ollama, LMStudio and other popular options.
- [ ] **In progress**: Update Readme, repo, org and website properly
- [ ] Audit (binary size, dependencies, test coverage, security, etc.).
- [ ] Check and sync all docs, validate all repo docs
- [ ] Release setup validation (website/CI/CD etc).
- [ ] Add llamastash to cli.rs https://github.com/zackify/cli.rs/pull/1/changes — Unit 7 cutover step, post-org-bootstrap.
- [ ] Add Agent Skills.
- [ ] **R1 launch promotion** — telling the world about v0.0.1.
  - [ ] **Need brainstorm/plan**: Release blog.
  - [ ] **Need brainstorm/research/plan**: Social promotion — research an approach for max reach.

### Follow-up

- [ ] **Release pipeline ops** — secret/token plumbing around `release.yml` and the org bootstrap.
  - [ ] Write `docs/runbooks/secret-rotation.md` — operational steps for rotating `CRATES_IO_TOKEN` + `GH_BUMP_TOKEN`. Referenced from [`docs/runbooks/release-0.0.1-bootstrap.md`](docs/runbooks/release-0.0.1-bootstrap.md) §"Token rotation cadence".
- [ ] Some HF downloaded models fail to start??
- [ ] random HF download failure ◓ Downloading 1/1 `Qwen_Qwen3.6-27B-Q8_0.gguf` (~27767.6 MiB) ✗ init download: hf-hub: request error: error sending request for url (https://huggingface.co/bartowski/Qwen_Qwen3.6-27B-GGUF/resolve/main/Qwen_Qwen3.6-27B-Q8_0.gguf): request error: error sending request for url (https://huggingface.co/bartowski/Qwen_Qwen3.6-27B-GGUF/resolve/main/Qwen_Qwen3.6-27B-Q8_0.gguf): error sending request for url (https://huggingface.co/bartowski/Qwen_Qwen3.6-27B-GGUF/resolve/main/Qwen_Qwen3.6-27B-Q8_0.gguf): client error (SendRequest): connection error: Connection timed out (os error 110)
- [ ]

### Good to have

- [ ] Mouse capture for pane focus and launch picker selection.
- [ ] Vim-style keybindings (h/j/k/l to navigate list, enter to launch, etc).

## R2 (post-v0.0.1 roadmap)

### Blockers

- [ ] `start` should support advanced params like TUI.
- [ ] **Deferred (post-c80d638)**: Port whichllm's family-selection / lineage-demotion / generation-bonus logic so `init --only models --json` output matches `whichllm --json --top 10` byte-for-byte. Today 7/10 picks and 3/10 quants match — see [Post-plan refinements §Remaining gap](docs/plans/2026-05-20-001-feat-live-hf-snapshot-discovery-plan.md#remaining-gap-deliberately-not-closed) in plan 2026-05-20-001.
- [ ] gpu/cpu offload split.
- [ ] **Need brainstorm/plan**: Plan to prevent llama.cpp version drift/incompatibility issues. Should we bundle/fix version.

### Follow-up

- [ ] **UAT follow-up** — items deferred from [`docs/plans/2026-05-19-002-feat-uat-e2e-hardware-strategy-plan.md`](docs/plans/2026-05-19-002-feat-uat-e2e-hardware-strategy-plan.md) that don't block R1 ship but are tracked against the UAT subsystem.
  - [ ] Lock in reference-model commit SHAs in `src/cli/uat/model.rs` — both `PRIMARY` and `FALLBACK` ship a `<TBD-locked-on-first-dry-run>` sentinel that the orchestrator surfaces as a `host.warnings` entry. First warm-mode dry-run on the maintainer's box lands the lock-in commit. Procedure: [`docs/runbooks/verify-uat-reintroduction.md`](docs/runbooks/verify-uat-reintroduction.md) §8b.
  - [ ] `Hardware UAT report` GitHub issue template — deferred until first contributor wants to file one (origin §Acceptance checklist). Recreate the `uat-caught` label if it's ever deleted: `gh label create uat-caught --color B60205 --description "Release PR where UAT caught a regression that would otherwise have shipped"`.
  - [ ] Cloud-runner re-evaluation — gated on user-base trigger (>500 installs + 3 RC cycles silence) per [`docs/plans/2026-05-19-002-feat-uat-e2e-hardware-strategy-plan.md`](docs/plans/2026-05-19-002-feat-uat-e2e-hardware-strategy-plan.md) §Companion trigger.
- [ ] **Release pipeline ops** (continued from R1).
  - [ ] **Need brainstorm/plan**: Migrate release pipeline secrets from PATs to a scoped GitHub App with OIDC. Eliminates `GH_BUMP_TOKEN` rotation and shrinks token blast radius. Deferred from 0.0.1 per the release-setup plan §"Token rotation surface".
- [ ] **Need brainstorm/plan**: Per-PID VRAM attribution via NVML's `nvmlDeviceGetComputeRunningProcesses` (Linux + Windows; AMD / Apple parity depends on upstream surface). Check ROCm and Metal for equivalents. Today the right-pane block title surfaces per-model RAM + CPU%; per-model VRAM is reported only at the host level.
- [ ] Make custom UI components reusable and consistent.

### Good to have

- [ ] **Deferred (verified 2026-05-21 against a real cache; not biting today)**: TUI list pane shows ambiguous file_stem labels for HF downloads. When a publisher uses a generic GGUF filename (`model.gguf`, `ggml-model-q4_k_m.gguf`), the list pane's `display_name(m) = file_stem(m.path)` renders two rows from different repos identically. The derived `<repo> (<quant>)` friendly-name slice (R118 / R119 / R120) was attempted and reverted in `2e11d65` because real catalogs use descriptive filenames. Revisit if a real catalog starts hitting the ambiguity — wire in a `list_models` lookup keyed by `header_blake3`. Origin: [`docs/plans/2026-05-20-002-feat-hf-pull-tui-dialog-plan.md`](docs/plans/2026-05-20-002-feat-hf-pull-tui-dialog-plan.md).
- [ ] **Need brainstorm/plan**: Windows support.
- [ ] **Need brainstorm/plan**: HTTP and MCP surfaces (origin: R34).
- [ ] **Need brainstorm/plan**: Anthropic API compatibility.
- [ ] **Need brainstorm/plan**: MLX and vLLM if cheap to add.
- [ ] **Need brainstorm/plan**: Docker-ready packaging.
