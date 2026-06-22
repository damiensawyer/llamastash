# Test coverage policy

LlamaStash aims for high line coverage on its testable logic and is honest
about what cannot be meaningfully unit-tested. We do not chase a single
headline number by writing trivial line-touching tests. A test earns its
place by pinning a behavior: an error branch, a parse edge, a precedence
rule, a wire-shape contract.

## How to measure

`make test-cov` (Tarpaulin, default ptrace engine) is authoritative for CI.
For a faster local loop the `cargo-llvm-cov` engine works too:

```bash
cargo llvm-cov --features test-fixtures --summary-only          # overall + per-file
cargo llvm-cov report --show-missing-lines \
  --ignore-filename-regex 'tests/'                              # exact uncovered lines
```

`--features test-fixtures` is required so the `fake_llama_server` fixture and
the `_test_sleep` IPC method compile (see AGENTS.md §Build, test, lint).

## Tiered targets

| Tier | Modules | Target |
|------|---------|--------|
| Pure logic | `gguf/`, `config/`, `launch/`, `discovery/` | ≈100% |
| Routing / protocol | `ipc/`, `proxy/route.rs`, `proxy/router.rs`, `init/` logic | ≈100% |
| Daemon orchestration | `daemon/` (supervisor, orphans, host_metrics, launch_service) | 90%+ |
| TUI / CLI logic (non-render) | `tui/*` logic, `cli/output.rs`, `cli/show.rs` | 85–95% |
| Render / IO / per-OS `#[cfg]` | see exclusion list below | best-effort |

## Honest exclusion list (best-effort tier)

These are not coverage gaps to close with synthetic tests — they are code
whose behavior only exists at a real terminal, a real subprocess boundary, or
a single OS. They are exercised by the pty harness (`scripts/tui/`), the
`--render` goldens, and the hardware UAT (`make uat-*`) rather than by unit
tests, and the uncovered residue is accepted.

- **TUI render functions** (`tui/render.rs`, the `*_pane.rs` widgets, overlays):
  draw to a `ratatui` buffer. Golden snapshots (`make render`) and the pty
  harness cover the observable output; the per-cell draw calls themselves are
  not unit-asserted.
- **Interactive wizard prompts** (`init/wizard.rs`, `init/prompts.rs`,
  `cli/init.rs`, `cli/picker.rs`): `cliclack` prompts block on a real TTY. The
  non-interactive `--recommended` paths are tested; the interactive prompt
  bodies are not.
- **Installer subprocess code** (`init/install/*`): spawns package managers
  (`brew`, `apt`, …) and downloads release assets. Covered by manual install
  flows and the UAT, not by unit tests.
- **CLI command entry points** (`cli/*` `run`/dispatch fns): thin glue that
  parses args, calls a tested helper, and maps the result to an exit code.
  The helpers and the exit-code mapping are tested; the dispatch wiring is not.
- **Per-OS `#[cfg]` arms** (e.g. the non-unix `is_executable` in
  `launch/binary.rs`, Windows-only path handling): only one arm compiles per
  target, so the other is structurally unreachable in any single CI run.
- **Defensive log-only branches**: e.g. `daemon/context.rs::mutate`'s
  `state_store::save` failure `log::warn!`, which fires only when the state
  directory becomes unwritable mid-run.

When adding code that falls in one of these categories, do not contort a test
to reach it — extend the relevant golden / pty / UAT coverage where it makes
sense, and leave the line uncovered rather than gaming the metric.
