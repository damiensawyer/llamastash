#!/usr/bin/env python3

from __future__ import annotations

import json
import sys
import xml.etree.ElementTree as ET
from pathlib import Path


def require_file(path: Path) -> Path:
  if not path.is_file():
    raise SystemExit(f"missing {path}; run 'make audit' first")
  return path


def format_mib(byte_count: int) -> str:
  return f"{byte_count / (1024 * 1024):.2f} MiB"


def duplicate_roots(text: str) -> int:
  count = 0
  for line in text.splitlines():
    if not line.strip():
      continue
    if line[0] in (" ", "│", "├", "└"):
      continue
    count += 1
  return count


def geiger_project_row(text: str) -> str:
  for line in text.splitlines():
    if "llamastash 0.0.1-beta.0" not in line:
      continue
    stripped = line.lstrip()
    if stripped.startswith(("Checking ", "├", "└", "│")):
      continue
    return line.strip()
  return "llamastash row not found"


def main() -> int:
  audit_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("target/audit")
  bytes_path = require_file(audit_dir / "release-binary-bytes.txt")
  audit_json_path = require_file(audit_dir / "cargo-audit.json")
  duplicates_path = require_file(audit_dir / "cargo-tree-duplicates.txt")
  geiger_path = require_file(audit_dir / "cargo-geiger.txt")
  geiger_exit_path = require_file(audit_dir / "cargo-geiger.exit-code.txt")
  cobertura_path = require_file(audit_dir / "tarpaulin" / "cobertura.xml")

  byte_count = int(bytes_path.read_text().strip())
  audit_json = json.loads(audit_json_path.read_text())
  duplicate_count = duplicate_roots(duplicates_path.read_text())
  geiger_output = geiger_path.read_text()
  geiger_exit = int(geiger_exit_path.read_text().strip())

  coverage = ET.parse(cobertura_path).getroot()
  lines_covered = int(coverage.attrib["lines-covered"])
  lines_valid = int(coverage.attrib["lines-valid"])
  coverage_pct = float(coverage.attrib["line-rate"]) * 100.0

  vuln_count = int(audit_json["vulnerabilities"]["count"])
  dep_count = int(audit_json.get("lockfile", {}).get("dependency-count", 0))

  print(f"Audit dir: {audit_dir}")
  print(f"Release binary: {byte_count} bytes ({format_mib(byte_count)})")
  print(f"Dependencies scanned by cargo-audit: {dep_count}")
  print(f"Known vulnerabilities: {vuln_count}")
  print(f"Duplicate dependency roots: {duplicate_count}")
  print(f"Geiger exit code: {geiger_exit}")
  print(f"Geiger project row: {geiger_project_row(geiger_output)}")
  print(f"Coverage: {lines_covered} / {lines_valid} lines ({coverage_pct:.2f}%)")

  if geiger_exit != 0:
    print(f"Geiger note: see {geiger_path} for upstream scan warnings")

  return 0


if __name__ == "__main__":
  raise SystemExit(main())
