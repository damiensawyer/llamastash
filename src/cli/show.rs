//! `llamastash show <model> [--json]`.
//!
//! One-stop projection of everything LlamaStash knows about a single
//! model: catalog row, GGUF metadata, on-disk size (summed across
//! split shards), the yaml + built-in `arch_defaults` that would
//! feed a launch, and the last `start_model` params recorded for
//! this file. Reuses the same resolver `start` and `/v1/...` use, so
//! a reference that works on one surface works here.

use serde_json::{json, Value};

use std::path::{Path, PathBuf};

use crate::cli::cli_args::{Cli, ShowArgs};
use crate::cli::client::connect_or_spawn;
use crate::cli::colors;
use crate::cli::exit_codes::{CliExit, CliResult};
use crate::cli::output::pretty_json;
use crate::cli::resolve::{fetch_catalog, resolve_model, CatalogRow};
use crate::config::Config;
use crate::daemon::host_metrics::GpuFlavor;
use crate::discovery::shard_sizes::{self, ShardSize};
use crate::launch::defaults_table;

pub async fn handle(args: ShowArgs, cli: &Cli, config: &Config) -> CliResult {
  // Every CLI command must support `--json`. Errors flow through the
  // same machinery: when `--json` is set, a CliExit lands on stdout
  // as `{"error": {"code": …, "message": …}}` instead of stderr
  // prose so agents can parse failure shapes without scraping. The
  // exit code is preserved either way.
  match build_view(&args, cli, config).await {
    Ok(view) => {
      if args.json {
        println!("{}", pretty_json(&view.envelope));
      } else {
        print!(
          "{}",
          render_human(&view.row, &view.shards, view.total_bytes, &view.envelope)
        );
      }
      Ok(())
    }
    Err(exit) => {
      if args.json {
        let body = json!({
          "error": {
            "code": exit.code,
            "message": exit.message.as_deref().unwrap_or(""),
          },
        });
        println!("{}", pretty_json(&body));
        // Drop the message so `report` doesn't double-print it to
        // stderr — the JSON body on stdout is the canonical surface.
        Err(crate::cli::exit_codes::CliExit::code_only(exit.code))
      } else {
        Err(exit)
      }
    }
  }
}

struct ShowView {
  row: CatalogRow,
  shards: Vec<ShardSize>,
  total_bytes: u64,
  envelope: Value,
}

async fn build_view(args: &ShowArgs, cli: &Cli, config: &Config) -> Result<ShowView, CliExit> {
  let mut client = connect_or_spawn(cli, config).await?;
  let catalog = fetch_catalog(&mut client).await?;
  let row = resolve_model(&catalog, &args.model)?;

  // Pull last-params for this model_path. The IPC handler keys by
  // ModelId; `model_path` is part of the JSON wire shape (`entry.id.path`)
  // and is unique within the catalog, so filtering by string equality
  // is sufficient here.
  let last_params_body = client
    .call("last_params_list", None)
    .await
    .map_err(CliExit::from_client_error)?;
  let last_params = last_params_body
    .get("last_params")
    .and_then(Value::as_array)
    .and_then(|rows| {
      rows.iter().find_map(|r| {
        let p = r.get("model_path").and_then(Value::as_str)?;
        if p == row.path {
          r.get("params").cloned()
        } else {
          None
        }
      })
    });

  // GPU backend from the daemon's host-metrics sampler — keys the
  // built-in arch_defaults lookup so the values we display match
  // what `start_model` would resolve.
  let status_body = client
    .call("status", None)
    .await
    .map_err(CliExit::from_client_error)?;
  let backend_label = status_body
    .get("host")
    .and_then(|h| h.get("gpu_backend"))
    .and_then(Value::as_str)
    .unwrap_or("");
  let backend = GpuFlavor::from_label(backend_label);

  // Built-in arch defaults for this (arch, backend) pair — the same
  // values that ship under `LayerLabel::ArchDefault` in the launch
  // resolver. Yaml arch_defaults sit on the same layer and win
  // per-field; surface both so the user sees where each field comes
  // from.
  let arch_key = row.arch.as_deref().unwrap_or("");
  let builtin_arch_defaults = defaults_table::lookup(arch_key, backend);
  let yaml_arch_defaults = row
    .arch
    .as_deref()
    .and_then(|a| config.arch_defaults.get(a))
    .cloned();

  let shards = shard_breakdown(&row);
  let total_bytes: u64 = shards
    .iter()
    .map(|s| s.bytes)
    .fold(0u64, u64::saturating_add);
  let shards_json: Vec<Value> = shards
    .iter()
    .enumerate()
    .map(|(idx, s)| {
      json!({
        "index": idx + 1,
        "path": s.path,
        "bytes": s.bytes,
      })
    })
    .collect();

  let envelope = json!({
    "name": row.name(),
    "path": row.path,
    "parent": row.parent,
    "source": row.source,
    // Backend that serves this model (R14 badge), derived from the source.
    "backend": crate::cli::output::backend_for_source(&row.source),
    "model_id": row.model_id,
    "display_label": row.display_label,
    "parse_error": row.parse_error,
    "metadata": {
      "arch": row.arch,
      "quant": row.quant,
      "native_ctx": row.native_ctx,
      "mode_hint": row.mode_hint,
      "parameter_label": row.parameter_label,
      "total_parameters": row.total_parameters,
      "tokenizer_kind": row.tokenizer_kind,
      "has_chat_template": row.has_chat_template,
      "has_reasoning_hint": row.has_reasoning_hint,
    },
    "size": {
      "weights_bytes": row.weights_bytes,
      "shard_count": shards.len(),
      "on_disk_total_bytes": total_bytes,
      "shards": shards_json,
    },
    "arch_defaults": {
      "gpu_backend": format!("{backend:?}"),
      "yaml": yaml_arch_defaults,
      "builtin": builtin_arch_defaults,
    },
    "last_params": last_params,
  });

  Ok(ShowView {
    row,
    shards,
    total_bytes,
    envelope,
  })
}

/// Per-shard `(path, bytes)` breakdown for the resolved row. Always
/// includes shard 1 (the catalog row's `path`); for split entries
/// extends with each sibling. Delegates to the shared
/// `discovery::shard_sizes` util so the byte counts here match the
/// values the scanner folded into `metadata.weights_bytes`.
fn shard_breakdown(row: &CatalogRow) -> Vec<ShardSize> {
  let primary = PathBuf::from(&row.path);
  let siblings: Vec<PathBuf> = row.split_siblings.iter().map(PathBuf::from).collect();
  shard_sizes::per_shard(&primary, &siblings)
}

fn render_human(row: &CatalogRow, shards: &[ShardSize], total_bytes: u64, env: &Value) -> String {
  use std::fmt::Write;
  let mut out = String::new();
  let kv = |buf: &mut String, key: &str, val: &str| {
    let _ = writeln!(buf, "  {}  {}", colors::dim(&format!("{key:<18}")), val);
  };

  let _ = writeln!(out, "{}", bold(&row.name()));
  // `path` covers single-file models; multi-shard sets get a full
  // per-shard listing under the `size` section below, so emit the
  // parent dir instead — shard 1's path on its own would only
  // partially describe the model on disk.
  if shards.len() == 1 {
    kv(&mut out, "path", &row.path);
  }
  kv(&mut out, "parent", &row.parent);
  kv(&mut out, "source", &row.source);
  kv(
    &mut out,
    "backend",
    crate::cli::output::backend_for_source(&row.source),
  );
  if let Some(id) = &row.model_id {
    kv(&mut out, "model_id", id);
  }
  if let Some(lbl) = &row.display_label {
    kv(&mut out, "display_label", lbl);
  }
  if let Some(err) = &row.parse_error {
    kv(&mut out, "parse_error", &colors::warning(err));
  }

  let _ = writeln!(out, "\n{}", bold("metadata"));
  kv(&mut out, "arch", row.arch.as_deref().unwrap_or("—"));
  kv(&mut out, "quant", row.quant.as_deref().unwrap_or("—"));
  kv(
    &mut out,
    "native_ctx",
    &row
      .native_ctx
      .map(|n| n.to_string())
      .unwrap_or_else(|| "—".into()),
  );
  kv(
    &mut out,
    "mode_hint",
    row.mode_hint.as_deref().unwrap_or("—"),
  );
  kv(
    &mut out,
    "parameter_label",
    row.parameter_label.as_deref().unwrap_or("—"),
  );
  kv(
    &mut out,
    "tokenizer_kind",
    row.tokenizer_kind.as_deref().unwrap_or("—"),
  );
  kv(
    &mut out,
    "has_chat_template",
    if row.has_chat_template { "yes" } else { "no" },
  );
  kv(
    &mut out,
    "has_reasoning_hint",
    if row.has_reasoning_hint { "yes" } else { "no" },
  );

  let _ = writeln!(out, "\n{}", bold("size"));
  kv(&mut out, "shard_count", &shards.len().to_string());
  kv(&mut out, "on_disk_total", &format_bytes(total_bytes));
  // Per-shard breakdown so a multi-shard model shows every file
  // and its individual size, not just shard 1. Single-file models
  // collapse to one row, keeping the human output tight.
  for (idx, shard) in shards.iter().enumerate() {
    let label = format!("shard {}", idx + 1);
    let size = if shard.bytes == 0 {
      colors::warning("missing")
    } else {
      format_bytes(shard.bytes)
    };
    let path = render_shard_path(&shard.path);
    kv(&mut out, &label, &format!("{size}  {path}"));
  }

  let backend = env
    .get("arch_defaults")
    .and_then(|a| a.get("gpu_backend"))
    .and_then(Value::as_str)
    .unwrap_or("");
  let _ = writeln!(
    out,
    "\n{} ({})",
    bold("arch_defaults"),
    colors::dim(backend),
  );
  let yaml = env.get("arch_defaults").and_then(|a| a.get("yaml"));
  let builtin = env.get("arch_defaults").and_then(|a| a.get("builtin"));
  kv(&mut out, "yaml", &knobs_one_line(yaml));
  kv(&mut out, "builtin", &knobs_one_line(builtin));

  let _ = writeln!(out, "\n{}", bold("last_params"));
  match env.get("last_params") {
    Some(Value::Null) | None => kv(&mut out, "(none)", "launch it once to populate"),
    Some(v) => kv(&mut out, "ctx", &fmt_field(v.get("ctx"))),
  }
  if let Some(v) = env.get("last_params") {
    if !v.is_null() {
      kv(&mut out, "mode", &fmt_field(v.get("mode")));
      kv(&mut out, "reasoning", &fmt_field(v.get("reasoning")));
      kv(&mut out, "knobs", &knobs_one_line(v.get("knobs")));
    }
  }

  out
}

fn fmt_field(v: Option<&Value>) -> String {
  match v {
    Some(Value::Null) | None => "—".into(),
    Some(Value::String(s)) => s.clone(),
    Some(other) => other.to_string(),
  }
}

fn knobs_one_line(value: Option<&Value>) -> String {
  let Some(Value::Object(map)) = value else {
    return "—".into();
  };
  let mut pairs: Vec<String> = map
    .iter()
    .filter(|(_, val)| !val.is_null())
    .map(|(key, val)| match val {
      Value::String(s) => format!("{key}={s}"),
      _ => format!("{key}={val}"),
    })
    .collect();
  pairs.sort();
  if pairs.is_empty() {
    "—".into()
  } else {
    pairs.join(", ")
  }
}

fn bold(s: &str) -> String {
  console::style(s).bold().to_string()
}

/// Friendly per-shard path: keep just the file basename — the row
/// header already showed the parent dir, so repeating the full path
/// per shard would wrap the line and bury the size column.
fn render_shard_path(path: &Path) -> String {
  path
    .file_name()
    .map(|s| s.to_string_lossy().into_owned())
    .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn format_bytes(n: u64) -> String {
  const KIB: f64 = 1024.0;
  const MIB: f64 = KIB * 1024.0;
  const GIB: f64 = MIB * 1024.0;
  let nf = n as f64;
  if nf >= GIB {
    format!("{:.2} GiB", nf / GIB)
  } else if nf >= MIB {
    format!("{:.1} MiB", nf / MIB)
  } else if nf >= KIB {
    format!("{:.0} KiB", nf / KIB)
  } else {
    format!("{n} B")
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  fn fake_row(path: &str) -> CatalogRow {
    CatalogRow {
      path: path.into(),
      model_id: Some("deadbeef".into()),
      parent: "/m".into(),
      source: "user".into(),
      arch: Some("qwen3".into()),
      quant: Some("Q5_K".into()),
      native_ctx: Some(32768),
      mode_hint: Some("chat".into()),
      parameter_label: Some("80B".into()),
      weights_bytes: Some(40_000_000_000),
      display_label: None,
      parse_error: None,
      split_siblings: vec![format!("{path}.part2"), format!("{path}.part3")],
      has_chat_template: true,
      has_reasoning_hint: false,
      tokenizer_kind: Some("qwen2".into()),
      total_parameters: Some(80_000_000_000),
    }
  }

  #[test]
  fn shard_breakdown_lists_every_shard_with_its_individual_size() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("m-00001-of-00002.gguf");
    std::fs::write(&p, b"1234567890").unwrap(); // 10 bytes
    let s2 = dir.path().join("m-00002-of-00002.gguf");
    std::fs::write(&s2, b"abcdef").unwrap(); // 6 bytes
    let row = CatalogRow {
      path: p.display().to_string(),
      split_siblings: vec![s2.display().to_string()],
      ..fake_row("/m/x.gguf")
    };
    let shards = shard_breakdown(&row);
    assert_eq!(shards.len(), 2);
    assert_eq!(shards[0].path, p);
    assert_eq!(shards[0].bytes, 10);
    assert_eq!(shards[1].path, s2);
    assert_eq!(shards[1].bytes, 6);
  }

  #[test]
  fn shard_breakdown_surfaces_missing_siblings_as_zero_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("present.gguf");
    std::fs::write(&p, b"0123").unwrap();
    let row = CatalogRow {
      path: p.display().to_string(),
      split_siblings: vec!["/does/not/exist.gguf-2".into()],
      ..fake_row("/m/x.gguf")
    };
    let shards = shard_breakdown(&row);
    assert_eq!(shards.len(), 2);
    assert_eq!(shards[0].bytes, 4);
    assert_eq!(shards[1].bytes, 0, "missing sibling renders as 0 not panic");
  }

  #[test]
  fn render_human_lists_every_shard_for_multipart() {
    // Regression: previous render only emitted shard 1's path under
    // the row header; siblings appeared as bare paths without sizes.
    // The size section must now show one row per shard with its
    // individual byte count.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("m-00001-of-00002.gguf");
    std::fs::write(&p, vec![0u8; 1024 * 1024]).unwrap(); // 1 MiB
    let s2 = dir.path().join("m-00002-of-00002.gguf");
    std::fs::write(&s2, vec![0u8; 2 * 1024 * 1024]).unwrap(); // 2 MiB
    let row = CatalogRow {
      path: p.display().to_string(),
      split_siblings: vec![s2.display().to_string()],
      ..fake_row("/m/x.gguf")
    };
    let shards = shard_breakdown(&row);
    let envelope = json!({
      "size": { "on_disk_total_bytes": 3 * 1024 * 1024 },
      "arch_defaults": { "gpu_backend": "CpuOnly", "yaml": null, "builtin": {} },
      "last_params": null,
    });
    let rendered =
      console::strip_ansi_codes(&render_human(&row, &shards, 3 * 1024 * 1024, &envelope))
        .into_owned();
    assert!(
      rendered.contains("shard 1"),
      "shard 1 row missing:\n{rendered}"
    );
    assert!(
      rendered.contains("shard 2"),
      "shard 2 row missing:\n{rendered}"
    );
    assert!(
      rendered.contains("1.0 MiB"),
      "shard 1 size missing:\n{rendered}"
    );
    assert!(
      rendered.contains("2.0 MiB"),
      "shard 2 size missing:\n{rendered}"
    );
    assert!(
      rendered.contains("m-00001-of-00002.gguf"),
      "shard 1 basename missing:\n{rendered}"
    );
    assert!(
      rendered.contains("m-00002-of-00002.gguf"),
      "shard 2 basename missing:\n{rendered}"
    );
    // Single `path` line should NOT appear in the row header for
    // multipart entries — the per-shard rows cover the same ground.
    assert!(
      !rendered.contains(&format!("path  {}", p.display())),
      "multipart should not duplicate shard 1 path under the header:\n{rendered}"
    );
  }

  #[test]
  fn format_bytes_rolls_through_units() {
    assert_eq!(format_bytes(0), "0 B");
    assert_eq!(format_bytes(1023), "1023 B");
    assert_eq!(format_bytes(1024), "1 KiB");
    assert!(format_bytes(2 * 1024 * 1024).starts_with("2.0 MiB"));
    assert!(format_bytes(3 * 1024 * 1024 * 1024).starts_with("3.00 GiB"));
  }

  #[test]
  fn knobs_one_line_sorts_keys_and_drops_nulls() {
    let v = json!({
      "ctx": 8192,
      "reasoning": null,
      "n_gpu_layers": 99,
      "flash_attn": true,
    });
    let line = knobs_one_line(Some(&v));
    assert!(!line.contains("reasoning"));
    assert!(line.contains("ctx=8192"));
    assert!(line.contains("flash_attn=true"));
    assert!(line.contains("n_gpu_layers=99"));
    // Sorted alphabetically: ctx < flash_attn < n_gpu_layers.
    let ctx_idx = line.find("ctx=").unwrap();
    let flash_idx = line.find("flash_attn=").unwrap();
    let ngl_idx = line.find("n_gpu_layers=").unwrap();
    assert!(ctx_idx < flash_idx && flash_idx < ngl_idx);
  }

  #[test]
  fn knobs_one_line_returns_dash_for_empty_or_null() {
    assert_eq!(knobs_one_line(None), "—");
    assert_eq!(knobs_one_line(Some(&Value::Null)), "—");
    assert_eq!(knobs_one_line(Some(&json!({}))), "—");
    // All-null map collapses to dash too.
    assert_eq!(
      knobs_one_line(Some(&json!({"ctx": null, "reasoning": null}))),
      "—"
    );
  }
}
