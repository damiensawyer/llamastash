---
title: "feat: multi-GPU device selection via `--device` knob"
type: feat
status: active
date: 2026-06-05
origin: user request (multi-GPU Vulkan split layers)
---

# feat: multi-GPU device selection via `--device` knob

## Overview

Adds per-GPU device selection (`--device`) so the user can target a specific card instead of letting llama.cpp split layers across all GPUs. The TUI device picker lists each card by backend-prefixed name (e.g. `Vulkan0 (AMD Radeon RX 7900)`, `Nvidia0 (NVIDIA GeForce RTX 3080)`) and the selection persists via `last_params` — favorites, presets, and returning user all carry the knob forward.

## Problem

When llamastash launches `llama-server` with Vulkan (or any GPU backend) and two or more GPUs are present, llama-server **splits model layers across all GPUs** by default. This means your RTX and R9700 both load the model, wasting VRAM on both cards.

The user's own launcher (`run.sh`) works because it pins to one GPU:

```bash
DEV=(--device Vulkan0)  # ← explicit card selection
```

llamastash had no way to pass `--device` at all.

## Solution

Add a `device` typed knob (`TypedKnobs.device: Option<String>`) that:

1. **Lists all GPUs by name** in the TUI device picker (e.g., `Vulkan0 (AMD Radeon RX 7900)`, `Nvidia0 (NVIDIA GeForce RTX 3080)`).
2. Lets the user pick which GPU to target — stored in the picker's `user_knobs` and persisted via `last_params` (favorites, presets, returning user).
3. Strips the backend prefix before passing to `--device`:
   - CUDA/HIP: `Nvidia0` → `--device 0`
   - ROCm: `Amd0` → `--device 0`
   - Vulkan: `Vulkan0` → `--device Vulkan0` (Vulkan expects the full token)
4. Carries per-device VRAM in `HostMetricsSnapshot.devices: Option<Vec<DeviceRow>>` via IPC so the TUI knows what cards exist.

## Files changed

| File | Change |
|------|--------|
| `src/config/loader.rs` | `TypedKnobs.device: Option<String>` field |
| `src/daemon/host_metrics.rs` | `DeviceRow` struct, `devices` on `HostMetricsSnapshot`, `build_device_rows()` helper |
| `src/launch/flag_aliases.rs` | `KnobField::Device`, `ValueKind::Str`, alias `--device`/`-d` |
| `src/launch/params.rs` | `argvify` emission, `try_inherit_field`, `compose` backend-aware formatting |
| `src/launch/defaults_table.rs` | `merge()` carries `device` field |
| `src/tui/launch_picker.rs` | `devices` field on `LaunchPickerState`, `cycle_device()`, `DEVICE_PRESETS` |
| `src/tui/tabs/settings.rs` | `knob_label`, `format_persisted_knob_value`, `format_knob_value` for Device |
| `src/tui/events.rs` | `is_editable`, `open_focused_inline_edit`, `commit_inline_edit` for Device |
| `src/tui/app.rs` | `build_default_picker()` populates `devices` from `host_metrics.devices` |
| `src/daemon/supervisor.rs` | `compose()` call updated with backend param |
| `src/cli/tail_args.rs` | `apply_knob` handles `Device`/`Str` |

## Test impact

- **No regressions**: 1423 tests pass (same as baseline; 1 pre-existing env-dependent failure unrelated to this change).
- **Updated**: `knob_specs_pinned_order_covers_every_field` (adds `--device` to expected canonical list).
- **Updated**: `apply_knob_handles_every_spec_in_the_alias_table` (adds `Str` value handling).
- **Added**: `build_device_rows()` is a pure function with no existing test — the existing `host_metrics` tests validate the IPC serialization path via `sample_priming`.

## What's NOT in this PR

- Device picker in the `init` wizard (deferred to follow-up)
- `doctor` warning when selected device VRAM < model weights (deferred to follow-up)
- Auto-detect and suggest best card for a given model (deferred)
