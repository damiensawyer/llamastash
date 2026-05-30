#!/usr/bin/env python3
"""Unit tests for deployment/scoop/packager.py.

Run directly:
    python3 deployment/scoop/packager_test.py

Wired into .github/workflows/ci.yml as a release-readiness step so template
or argv regressions surface on every PR, not just at release time.

This file is excluded from the published crate via Cargo.toml's
`exclude = ["deployment/*"]`.
"""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import packager  # noqa: E402

TEMPLATE_PATH = str(HERE / "llamastash.json.template")


def _render(version="0.0.2", sha="a" * 64):
    out = tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False)
    out.close()
    rendered = packager.render(
        version=version,
        template_path=TEMPLATE_PATH,
        output_path=out.name,
        sha_x86_64_windows=sha,
    )
    return rendered, out.name


class TestRender(unittest.TestCase):
    def setUp(self):
        self.tempfiles = []

    def tearDown(self):
        for f in self.tempfiles:
            try:
                Path(f).unlink()
            except FileNotFoundError:
                pass

    def test_renders_valid_json(self):
        rendered, path = _render()
        self.tempfiles.append(path)
        # Must parse as JSON.
        manifest = json.loads(rendered)
        self.assertEqual(manifest["version"], "0.0.2")
        self.assertEqual(manifest["bin"], "llamastash.exe")
        # Architecture URL carries the version.
        url = manifest["architecture"]["64bit"]["url"]
        self.assertIn("v0.0.2", url)
        self.assertTrue(url.endswith("x86_64-pc-windows-msvc.zip"))
        # Hash is the sha we passed (64 'a's).
        self.assertEqual(manifest["architecture"]["64bit"]["hash"], "a" * 64)
        # extract_dir matches the asset name without the .zip.
        self.assertEqual(
            manifest["architecture"]["64bit"]["extract_dir"],
            "llamastash-0.0.2-x86_64-pc-windows-msvc",
        )

    def test_autoupdate_uses_scoop_dollar_version(self):
        # Scoop's own `$version` autoupdate placeholder must survive
        # substitution (Python sees `$$version` in the template, emits
        # `$version` literal — Scoop resolves that at autoupdate time).
        rendered, path = _render()
        self.tempfiles.append(path)
        manifest = json.loads(rendered)
        auto_url = manifest["autoupdate"]["architecture"]["64bit"]["url"]
        self.assertIn("$version", auto_url, "Scoop $version must survive")
        # Hash autoupdate URL points at the .sha256 sidecar.
        hash_url = manifest["autoupdate"]["hash"]["url"]
        self.assertEqual(hash_url, "$url.sha256")

    def test_strips_leading_v_from_version(self):
        rendered, path = _render(version="v0.0.2")
        self.tempfiles.append(path)
        manifest = json.loads(rendered)
        self.assertEqual(manifest["version"], "0.0.2")

    def test_rejects_invalid_version(self):
        with self.assertRaises(SystemExit):
            _render(version="not-a-version")

    def test_rejects_short_sha(self):
        with self.assertRaises(SystemExit):
            _render(sha="a" * 63)

    def test_rejects_non_hex_sha(self):
        with self.assertRaises(SystemExit):
            _render(sha=("z" * 64))


class TestCliShape(unittest.TestCase):
    def test_wrong_argc_exits(self):
        with self.assertRaises(SystemExit):
            packager.main(["packager.py"])  # too few args
        with self.assertRaises(SystemExit):
            packager.main(["packager.py", "0.0.2"])


if __name__ == "__main__":
    unittest.main()
