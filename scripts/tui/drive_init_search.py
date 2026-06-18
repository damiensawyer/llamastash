#!/usr/bin/env python3
"""Drive the `llamastash init` model picker's HuggingFace search flow.

Unlike `tui_drive.py` / `harness.py` (which drive the full-screen ratatui
TUI), `init` is a linear `cliclack` wizard — a different UI stack that the
pyte-only TUI drivers can't reach. This script gives `init --only models` a
real PTY, renders the live screen with pyte, and scripts the keystrokes to:

    Pick a model  →  "Search HuggingFace by name…"  →  query  →  results
                  →  "← Back to model list"  →  "Skip"

It is **non-destructive**: it ends on Skip, so no model is downloaded. The
search step does hit the network (the live HF `/api/models` endpoint).

Usage:
    python3 scripts/tui/drive_init_search.py [binary] [query]

    binary  path to the llamastash binary (default: target/debug/llamastash)
    query   search term to type (default: "qwen3")

Pair with an isolated state dir so it never touches your real daemon/config:
    export LLAMASTASH_STATE_DIR=$(mktemp -d) LLAMASTASH_CONFIG_DIR=$(mktemp -d)
    export LLAMASTASH_CACHE_DIR=$(mktemp -d) HF_HOME=$(mktemp -d)

Requires `pyte` + `pexpect` (see scripts/tui/README.md for the venv recipe).
"""

import os
import sys
import time

try:
    import pexpect
    import pyte
except ImportError:
    sys.exit("drive_init_search: needs `pyte` and `pexpect` (pip install pyte pexpect)")

BIN = sys.argv[1] if len(sys.argv) > 1 else "target/debug/llamastash"
QUERY = sys.argv[2] if len(sys.argv) > 2 else "qwen3"

COLS, ROWS = 160, 50
UP, DOWN, ENTER = "\x1b[A", "\x1b[B", "\r"

screen = pyte.Screen(COLS, ROWS)
stream = pyte.ByteStream(screen)
child = pexpect.spawn(
    BIN, ["init", "--only", "models"], env=dict(os.environ), dimensions=(ROWS, COLS), timeout=60
)


def pump(seconds):
    """Read PTY output into the pyte screen for `seconds`."""
    end = time.time() + seconds
    while time.time() < end:
        try:
            stream.feed(child.read_nonblocking(4096, timeout=0.3))
        except pexpect.TIMEOUT:
            pass
        except pexpect.EOF:
            break


def show(label):
    print(f"\n===== {label} =====")
    for line in screen.display:
        if line.strip():
            print(line)


# Boot: hardware detection + recommendation fetch, then the "Pick a model" list.
pump(20)
show("model-menu")

# Reach "Search HuggingFace by name…" (second-to-last item, before "Skip").
# Clamp to the bottom with many Downs, then step Up once — robust to however
# many recommendations the picker shows.
child.send(DOWN * 25)
time.sleep(0.5)
child.send(UP)
pump(1.5)
show("on-search-item")

child.send(ENTER)
pump(2)
show("query-prompt")

child.send(QUERY)
pump(1)
child.send(ENTER)
pump(15)  # live HF search
show("search-results")

# "← Back to model list" is the last results item — clamp down + Enter.
child.send(DOWN * 30)
time.sleep(0.4)
child.send(ENTER)
pump(2)
show("back-to-menu")

# "Skip" is the last model-menu item — clamp down + Enter. No download.
child.send(DOWN * 30)
time.sleep(0.4)
child.send(ENTER)
pump(3)
show("after-skip")

child.close(force=True)
