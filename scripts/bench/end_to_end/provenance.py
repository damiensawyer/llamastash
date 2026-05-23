"""Best-effort capture of host + tool-version provenance.

Every value is "captured if possible, recorded as ``None`` if not."
The capture functions never raise — missing binaries, permission
errors, or unexpected output formats all degrade gracefully to
``None`` and the harness keeps going. The renderer reports gaps in
the published page so readers see exactly what the maintainer's
machine could and couldn't see.

Q4 (per the requirements doc): the llama.cpp commit string is
captured best-effort. `llama-server --version` reliably embeds it;
Ollama's `ollama --version` reports its own version + a vendored
llama.cpp SHA on a separate line; LM Studio's `lms version` is
recorded verbatim and not parsed further. Anything we can't extract
cleanly stays `None` rather than guessing.
"""
from __future__ import annotations

import os
import platform
import re
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Optional

from .schema import Host, Provenance


def _run(cmd: list[str], timeout_s: float = 5.0) -> Optional[str]:
  """Run `cmd`, return stdout (stripped) on success, None on any
  failure. Intentionally swallows every exception — provenance must
  never abort a benchmark run.

  `cmd[0]` is resolved via `shutil.which` first to give a clean
  "tool missing" → None path without raising FileNotFoundError.
  """
  if not cmd:
    return None
  if shutil.which(cmd[0]) is None:
    return None
  try:
    result = subprocess.run(
      cmd,
      capture_output=True,
      text=True,
      timeout=timeout_s,
      check=False,
    )
  except (subprocess.TimeoutExpired, OSError):
    return None
  out = (result.stdout or "") + (result.stderr or "")
  out = out.strip()
  return out or None


# ---- Host info ---------------------------------------------------


def _short_hostname() -> str:
  """Lowercase short hostname, alnum-only fallback chain. Used as
  the `runs/<host-id>/` subdirectory — must be filesystem-safe."""
  raw = (
    os.environ.get("LLAMASTASH_BENCH_HOST_ID")
    or platform.node()
    or os.uname().nodename
    or "unknown"
  )
  short = raw.split(".", 1)[0].lower()
  return re.sub(r"[^a-z0-9_-]+", "-", short) or "unknown"


def _cpu_model() -> str:
  """Best-effort CPU model string. /proc/cpuinfo on Linux,
  sysctl on macOS, platform.processor() as last resort."""
  cpuinfo = Path("/proc/cpuinfo")
  if cpuinfo.exists():
    try:
      for line in cpuinfo.read_text(errors="replace").splitlines():
        if line.lower().startswith("model name"):
          return line.split(":", 1)[1].strip()
    except OSError:
      pass
  out = _run(["sysctl", "-n", "machdep.cpu.brand_string"])
  if out:
    return out
  return platform.processor() or platform.machine() or "unknown"


def _cpu_threads() -> int:
  return os.cpu_count() or 1


def _ram_gb() -> float:
  """Total RAM in GiB. /proc/meminfo on Linux, sysctl on macOS, 0
  as a falsy sentinel when neither path works."""
  meminfo = Path("/proc/meminfo")
  if meminfo.exists():
    try:
      for line in meminfo.read_text(errors="replace").splitlines():
        if line.startswith("MemTotal:"):
          kb = int(line.split()[1])
          return round(kb / (1024 * 1024), 2)
    except (OSError, ValueError, IndexError):
      pass
  out = _run(["sysctl", "-n", "hw.memsize"])
  if out and out.isdigit():
    return round(int(out) / (1024**3), 2)
  return 0.0


def _detect_gpu_backend() -> str:
  """Return the most likely backend name. Linux + macOS only; mirrors
  the labels in measure-overhead-band.sh."""
  if os.environ.get("LLAMASTASH_BENCH_GPU_BACKEND"):
    return os.environ["LLAMASTASH_BENCH_GPU_BACKEND"].lower()
  if platform.system() == "Darwin":
    return "metal"
  if shutil.which("nvidia-smi"):
    return "cuda"
  if shutil.which("rocm-smi") or shutil.which("rocminfo"):
    return "rocm"
  if shutil.which("vulkaninfo"):
    return "vulkan"
  return "cpu"


def _detect_gpu_name(backend: str) -> Optional[str]:
  if backend == "cuda":
    out = _run(["nvidia-smi", "--query-gpu=name", "--format=csv,noheader"])
    if out:
      return out.splitlines()[0].strip()
  if backend == "rocm":
    out = _run(["rocm-smi", "--showproductname"])
    if out:
      for line in out.splitlines():
        # `rocm-smi --showproductname` prints
        # `GPU[0]\t\t: Card Series: \t\tAMD Radeon ...`
        # — match case-insensitively and on `Card Series` first.
        if "card series" in line.lower():
          return line.split(":", 2)[-1].strip()
      for line in out.splitlines():
        if "card model" in line.lower():
          return line.split(":", 2)[-1].strip()
  if backend == "metal":
    out = _run(["system_profiler", "SPDisplaysDataType"], timeout_s=10.0)
    if out:
      for line in out.splitlines():
        s = line.strip()
        if s.startswith("Chipset Model:"):
          return s.split(":", 1)[1].strip()
  return None


def _detect_gpu_vram_gb(backend: str) -> Optional[float]:
  if backend == "cuda":
    out = _run(["nvidia-smi", "--query-gpu=memory.total", "--format=csv,noheader,nounits"])
    if out:
      try:
        return round(int(out.splitlines()[0].strip()) / 1024, 2)
      except ValueError:
        return None
  if backend == "rocm":
    out = _run(["rocm-smi", "--showmeminfo", "vram"])
    if out:
      for line in out.splitlines():
        m = re.search(r"VRAM Total Memory \(B\):\s*(\d+)", line)
        if m:
          return round(int(m.group(1)) / (1024**3), 2)
  return None


def capture_host() -> Host:
  backend = _detect_gpu_backend()
  return Host(
    host_id=_short_hostname(),
    os=f"{platform.system()} {platform.release()}",
    cpu=_cpu_model(),
    cpu_threads=_cpu_threads(),
    ram_gb=_ram_gb(),
    gpu_backend=backend,  # type: ignore[arg-type]
    gpu_name=_detect_gpu_name(backend),
    gpu_vram_gb=_detect_gpu_vram_gb(backend),
  )


# ---- Tool versions ------------------------------------------------

_LLAMA_CPP_COMMIT_RE = re.compile(
  r"(?:llama\.cpp|server|version):?\s*([0-9a-f]{7,40})", re.IGNORECASE
)


def _extract_llama_cpp_commit(version_output: str) -> Optional[str]:
  """`llama-server --version` typically prints something like:
      version: 3705 (b6e7c5a)
      built with cc (GCC) ...
  Extract the SHA from the parenthesised tail when present; fall
  back to the regex match. Returns None when neither path hits."""
  m = re.search(r"\(([0-9a-f]{7,40})\)", version_output)
  if m:
    return m.group(1)
  m = _LLAMA_CPP_COMMIT_RE.search(version_output)
  if m:
    return m.group(1)
  return None


def _capture_version(binary: str, args: Optional[list[str]] = None) -> Optional[str]:
  args = args or ["--version"]
  out = _run([binary, *args])
  if not out:
    return None
  return out.splitlines()[0].strip()


def capture_provenance() -> Provenance:
  llamastash = _capture_version("llamastash")
  llama_server_raw = _run(["llama-server", "--version"])
  llama_server = llama_server_raw.splitlines()[0].strip() if llama_server_raw else None
  llama_cpp_commit = _extract_llama_cpp_commit(llama_server_raw) if llama_server_raw else None

  ollama_raw = _run(["ollama", "--version"])
  ollama = ollama_raw.splitlines()[0].strip() if ollama_raw else None
  ollama_llama_cpp_commit = (
    _extract_llama_cpp_commit(ollama_raw) if ollama_raw else None
  )

  # LM Studio's `lms` CLI prints its own version; we don't parse it
  # further. Recorded as-is.
  lmstudio = _capture_version("lms", ["version"]) or _capture_version("lms")

  return Provenance(
    llamastash_version=llamastash,
    llama_server_version=llama_server,
    llama_cpp_commit=llama_cpp_commit,
    ollama_version=ollama,
    ollama_llama_cpp_commit=ollama_llama_cpp_commit,
    lmstudio_version=lmstudio,
    python_version=sys.version.split()[0],
  )


__all__ = [
  "capture_host",
  "capture_provenance",
]
