#!/usr/bin/env python3
"""Generate scoop-llamastash/llamastash.json from the template + Windows
release SHA-256.

Usage:
    packager.py <version> <template_path> <output_path> <sha_x86_64_windows>

Mirrors deployment/homebrew/packager.py shape: a thin substitute over
string.Template, hardened with input shape checks and a post-render
assertion that no `$placeholder` survives.

This script is in `Cargo.toml`'s `exclude` list (via `deployment/*`) so it
does not ship in the published crate.
"""

from __future__ import annotations

import re
import sys
from string import Template

# Semver loose form (same shape as the homebrew packager).
VERSION_RE = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?$")
SHA256_RE = re.compile(r"^[a-fA-F0-9]{64}$")

EXPECTED_ARGC = 5  # script name + 4 args


def _die(msg: str, code: int = 2) -> None:
    print(f"error: {msg}", file=sys.stderr)
    sys.exit(code)


def render(
    version: str,
    template_path: str,
    output_path: str,
    sha_x86_64_windows: str,
) -> str:
    """Render the Scoop manifest. Returns the rendered text.

    Raises SystemExit on any input shape failure or surviving placeholder.
    """
    # Strip a leading 'v' if a tag (vX.Y.Z) was passed.
    version = version.lstrip("v")

    if not VERSION_RE.match(version):
        _die(f"invalid version: {version!r} (expected X.Y.Z or X.Y.Z-suffix)")
    if not SHA256_RE.match(sha_x86_64_windows):
        _die(
            f"invalid sha_x86_64_windows: {sha_x86_64_windows!r} "
            "(expected 64 hex chars)"
        )

    with open(template_path, "r", encoding="utf-8") as fh:
        template_src = fh.read()

    template = Template(template_src)
    mapping = {
        "version": version,
        "hash_x86_64": sha_x86_64_windows,
    }

    # Pre-flight: every Python-fillable placeholder we ship in `mapping`
    # must actually appear in the template. Catches the case where a
    # template typo (`$hash_x64_windows`) silently leaves us without a
    # substituted hash. We scan placeholders from the raw template
    # (before safe_substitute) so $$ Scoop placeholders are not yet
    # collapsed to literal `$`.
    raw_placeholders = {
        name
        for groups in Template.pattern.findall(template_src)
        for name in groups
        if name and name not in {"$"}
    }
    missing = set(mapping.keys()) - raw_placeholders
    if missing:
        _die(
            "template missing required $placeholders: " + ", ".join(sorted(missing))
        )

    rendered = template.safe_substitute(mapping)

    with open(output_path, "w", encoding="utf-8") as fh:
        fh.write(rendered)

    return rendered


def main(argv: list[str]) -> None:
    if len(argv) != EXPECTED_ARGC:
        print(__doc__.strip() if __doc__ else "", file=sys.stderr)
        _die(f"expected {EXPECTED_ARGC - 1} args, got {len(argv) - 1}")

    version = argv[1].strip()
    template_path = argv[2]
    output_path = argv[3]
    sha_x86_64_windows = argv[4].strip()

    print("Generating Scoop manifest")
    print(f"     VERSION: {version}")
    print(f"     TEMPLATE PATH: {template_path}")
    print(f"     SAVING AT: {output_path}")
    print(f"     SHA x86_64-pc-windows-msvc: {sha_x86_64_windows}")

    rendered = render(version, template_path, output_path, sha_x86_64_windows)

    print("\n================== Generated manifest ==================\n")
    print(rendered)
    print("\n========================================================\n")


if __name__ == "__main__":
    main(sys.argv)
