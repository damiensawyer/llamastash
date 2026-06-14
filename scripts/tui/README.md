# LlamaStash TUI drivers

Two ways to drive the full-screen TUI non-interactively. LlamaStash is a
`ratatui`/`crossterm` app, so you can't assert on its output by piping stdout —
both tools give it a real PTY, render the live screen with a terminal emulator
([pyte]), and hand you back plain text.

| Tool | Use it when |
|------|-------------|
| **`tui_drive.py`** | Quick, throwaway inspection. Zero deps beyond `pyte`, JSON-on-argv (easy for an agent to generate inline), prints each screen to stdout. No assertions, no exit code. Reach for this to *look* at a flow. |
| **`harness.py`** | Repeatable UAT / regression checks. Adds `expect`/`refute` assertions, PASS/FAIL accounting, a non-zero exit code for CI, persisted `snap:` screenshots, and mid-run re-`spawn:`. Reach for this to *gate* on a flow. Needs `pexpect` on top of `pyte`. |

Both inherit this process's env, so pair either with an isolated state dir
(`LLAMASTASH_STATE_DIR` + friends, see `../../AGENTS.md`) to drive a clean
daemon. Build first: `cargo build --bin llamastash`.

`harness.py` also answers crossterm's cursor-position query (`ESC[6n`) so the
app doesn't abort with "cursor position could not be read"; `tui_drive.py`
does not, so it can be more fragile depending on TUI init.

## Requirements

Python 3.9+. A throwaway venv keeps it off the system Python:

```bash
python3 -m venv /tmp/ls-tui-venv
/tmp/ls-tui-venv/bin/pip install pyte pexpect   # tui_drive.py only needs pyte
```

## tui_drive.py

```bash
python3 scripts/tui/tui_drive.py '[["", 4, "boot"], ["/gemma|<enter>", 2, "staged"]]'
```

A JSON array of `[keys, wait_seconds, label]` steps; `|` separates tokens in a
step; `<down> <up> <left> <right> <enter> <esc> <tab>` map to escape sequences.
See the script's docstring for the full contract.

## harness.py

```bash
# program file, outdir for snapshots, optional binary + extra args
/tmp/ls-tui-venv/bin/python scripts/tui/harness.py \
    scripts/tui/example.prog /tmp/ls-tui-out
```

- `program` — a step file (see below).
- `outdir` — where `snap:` writes `<label>.txt` screenshots.
- `binary` — defaults to `target/debug/llamastash`.
- `args...` — extra CLI args (default: none; the bare binary opens the TUI).

Exit code is non-zero if any `expect`/`refute` failed.

### Program steps

One step per line; blank lines and `#` comments are ignored.

| Step | Effect |
|------|--------|
| `spawn:<args>` | (Re)spawn llamastash with extra CLI args |
| `key:<name>` | Send named key(s), space-separated (see below) |
| `type:<text>` | Type literal characters |
| `wait:<seconds>` | Sleep while pumping PTY output into the screen |
| `settle` | Wait the default settle interval |
| `snap:<label>` | Save the current screen to `<outdir>/<label>.txt` |
| `expect:<substr>` | Assert the screen contains `substr` (PASS/FAIL) |
| `refute:<substr>` | Assert the screen does not contain `substr` |
| `iexpect:<substr>` | Case-insensitive `expect` |
| `comment:<text>` | Print a comment line |

### Key names

`enter esc tab backtab space up down left right home end pageup pagedown`
`ctrl-c ctrl-d ctrl-h ctrl-r`

Plain characters (letters, digits, `?`, `/`, `-`) are sent with `type:`.
Shift+letter is just the uppercase letter, e.g. `type:P` for `Shift+p`.

The screen is rendered at `160x45` to match the canonical `make render` size,
so `snap:` output lines up with the golden fixtures under `tests/golden/`.

[pyte]: https://github.com/selectel/pyte
