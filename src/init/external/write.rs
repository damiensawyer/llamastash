//! Format-aware read / merge / write glue.
//!
//! Both JSON and YAML targets route through the same merge logic
//! ([`super::merge`]) — only the read-current and serialise-final
//! steps care about format. The atomic write itself is
//! [`crate::util::atomic_write::write_secure`], shared with the
//! daemon state store and llamastash's own config writer.

use std::path::Path;

use serde_json::Value;

use crate::util::atomic_write::write_secure;
use crate::util::config_patch::DiffEntry;

use super::{merge, Format, PatchContext, PatchError, ToolPatcher};

/// Read the file at `path` and parse it according to `format`.
/// Missing or empty files return an empty JSON object (so a fresh
/// install gets clean "Added" diff rows). YAML files are parsed
/// via `serde_yaml` then converted into JSON via `serde_json::to_value`
/// so the merge stays JSON-native.
pub fn read_current(
  tool_id: &'static str,
  path: &Path,
  format: Format,
) -> Result<Value, PatchError> {
  let raw = match std::fs::read_to_string(path) {
    Ok(s) if s.trim().is_empty() => return Ok(Value::Object(Default::default())),
    Ok(s) => s,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
      return Ok(Value::Object(Default::default()))
    }
    Err(e) => {
      return Err(PatchError::Read {
        tool_id,
        path: path.to_path_buf(),
        error: e.to_string(),
      })
    }
  };
  match format {
    Format::Json => serde_json::from_str(&raw).map_err(|e| PatchError::Parse {
      tool_id,
      path: path.to_path_buf(),
      format,
      error: e.to_string(),
    }),
    Format::Yaml => {
      let yaml: serde_yaml::Value = serde_yaml::from_str(&raw).map_err(|e| PatchError::Parse {
        tool_id,
        path: path.to_path_buf(),
        format,
        error: e.to_string(),
      })?;
      serde_json::to_value(&yaml).map_err(|e| PatchError::Parse {
        tool_id,
        path: path.to_path_buf(),
        format,
        error: format!("yaml→json: {e}"),
      })
    }
    Format::Raw => Ok(Value::Object(Default::default())),
  }
}

/// Serialise the merged JSON value back into the target format's
/// canonical text. YAML uses block style for readability; JSON uses
/// pretty-print so the user can re-read what we wrote.
fn serialise(tool_id: &'static str, merged: &Value, format: Format) -> Result<String, PatchError> {
  match format {
    Format::Json => {
      let mut s = serde_json::to_string_pretty(merged)
        .map_err(|e| PatchError::Serialise(format!("{tool_id}: {e}")))?;
      // serde_json::to_string_pretty drops trailing newline; add one
      // back so editors don't flag the file with an end-of-file warning.
      s.push('\n');
      Ok(s)
    }
    Format::Yaml => {
      serde_yaml::to_string(merged).map_err(|e| PatchError::Serialise(format!("{tool_id}: {e}")))
    }
    Format::Raw => Err(PatchError::Serialise(format!(
      "{tool_id}: Format::Raw bypasses merge — caller must use apply_raw_body"
    ))),
  }
}

/// Compute the structural diff that [`apply_merge`] would produce —
/// without writing the file. Used by [`super::dry_run`].
pub fn compute_diff(
  patcher: &dyn ToolPatcher,
  ctx: &PatchContext,
  path: &Path,
  format: Format,
) -> Result<Vec<DiffEntry>, PatchError> {
  let current = read_current(patcher.id(), path, format)?;
  let merged = patcher.merge_with_current(current.clone(), ctx);
  Ok(merge::diff(&current, &merged))
}

/// Compute a diff for [`Format::Raw`] patchers: the entire body is
/// either Added (file missing or empty) or Changed (existing file
/// differs), with `path: "<file>"` as a single synthetic row. Lets
/// the same dry-run / apply rendering work for the env.sh writer.
pub fn compute_raw_diff(
  patcher: &dyn ToolPatcher,
  ctx: &PatchContext,
  path: &Path,
) -> Result<Vec<DiffEntry>, PatchError> {
  let body = patcher.raw_body(ctx).ok_or_else(|| {
    PatchError::Serialise(format!(
      "{}: Format::Raw patcher must implement raw_body()",
      patcher.id()
    ))
  })?;
  let current = match std::fs::read_to_string(path) {
    Ok(s) => Some(s),
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
    Err(e) => {
      return Err(PatchError::Read {
        tool_id: patcher.id(),
        path: path.to_path_buf(),
        error: e.to_string(),
      })
    }
  };
  use crate::util::config_patch::DiffKind;
  match current {
    Some(ref s) if s == &body => Ok(Vec::new()),
    Some(_) => Ok(vec![DiffEntry {
      path: file_label(path),
      kind: DiffKind::Changed,
      value_yaml: body.lines().count().to_string() + " line(s)",
    }]),
    None => Ok(vec![DiffEntry {
      path: file_label(path),
      kind: DiffKind::Added,
      value_yaml: body.lines().count().to_string() + " line(s)",
    }]),
  }
}

fn file_label(path: &Path) -> String {
  path
    .file_name()
    .and_then(|s| s.to_str())
    .unwrap_or("<file>")
    .to_string()
}

/// Read current, merge additions, atomic-write. Returns the diff
/// and `written_bytes`. Production hot path called by [`super::apply`].
pub fn apply_merge(
  patcher: &dyn ToolPatcher,
  ctx: &PatchContext,
  path: &Path,
  format: Format,
) -> Result<(Vec<DiffEntry>, u64), PatchError> {
  let current = read_current(patcher.id(), path, format)?;
  let merged = patcher.merge_with_current(current.clone(), ctx);
  let diff_rows = merge::diff(&current, &merged);
  let body = serialise(patcher.id(), &merged, format)?;
  let written = atomic_write_body(patcher, path, body.as_bytes())?;
  Ok((diff_rows, written))
}

/// Whole-file write for [`Format::Raw`] patchers (env.sh writer).
pub fn apply_raw(
  patcher: &dyn ToolPatcher,
  ctx: &PatchContext,
  path: &Path,
) -> Result<(Vec<DiffEntry>, u64), PatchError> {
  let body = patcher.raw_body(ctx).ok_or_else(|| {
    PatchError::Serialise(format!(
      "{}: Format::Raw patcher must implement raw_body()",
      patcher.id()
    ))
  })?;
  let diff_rows = compute_raw_diff(patcher, ctx, path)?;
  let written = atomic_write_body(patcher, path, body.as_bytes())?;
  Ok((diff_rows, written))
}

fn atomic_write_body(
  patcher: &dyn ToolPatcher,
  path: &Path,
  body: &[u8],
) -> Result<u64, PatchError> {
  let dir = path
    .parent()
    .unwrap_or_else(|| Path::new("."))
    .to_path_buf();
  let prefix = format!("{}.tmp.", patcher.id());
  write_secure(&dir, &prefix, path, body, Some(patcher.unix_mode())).map_err(|e| {
    PatchError::Write {
      tool_id: patcher.id(),
      path: path.to_path_buf(),
      error: e.to_string(),
    }
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::init::external::ToolPatcher;
  use std::path::PathBuf;

  struct YamlStub;

  impl ToolPatcher for YamlStub {
    fn id(&self) -> &'static str {
      "yaml-stub"
    }
    fn display_name(&self) -> &'static str {
      "YAML Stub"
    }
    fn default_path(&self) -> Option<PathBuf> {
      None
    }
    fn format(&self) -> Format {
      Format::Yaml
    }
    fn build_additions(&self, ctx: &PatchContext) -> Value {
      serde_json::json!({ "openai-api-base": ctx.proxy_base_url })
    }
  }

  fn ctx() -> PatchContext {
    PatchContext {
      proxy_base_url: "http://127.0.0.1:11435/v1".into(),
      api_key: "llamastash".into(),
      model_id: None,
    }
  }

  #[test]
  fn yaml_round_trip_preserves_user_keys() {
    let dir = crate::util::test_temp::unique_temp_dir("ext-write-yaml");
    let path = dir.join("conf.yaml");
    std::fs::write(&path, "model: gpt-4o\nfoo: bar\n").unwrap();
    let (diff_rows, written) = apply_merge(&YamlStub, &ctx(), &path, Format::Yaml).expect("apply");
    assert!(written > 0);
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.contains("openai-api-base: http://127.0.0.1:11435/v1"));
    assert!(body.contains("model: gpt-4o"));
    assert!(body.contains("foo: bar"));
    assert!(diff_rows.iter().any(|d| d.path == "openai-api-base"));
    std::fs::remove_dir_all(&dir).ok();
  }

  #[test]
  fn json_pretty_print_ends_with_newline() {
    let dir = crate::util::test_temp::unique_temp_dir("ext-write-json");
    let path = dir.join("conf.json");
    let s = serialise("t", &serde_json::json!({"a":1}), Format::Json).unwrap();
    assert!(s.ends_with('\n'));
    let _ = path;
    std::fs::remove_dir_all(&dir).ok();
  }

  #[test]
  fn read_current_treats_missing_file_as_empty_object() {
    let dir = crate::util::test_temp::unique_temp_dir("ext-write-missing");
    let path = dir.join("absent.json");
    let v = read_current("t", &path, Format::Json).unwrap();
    assert_eq!(v, Value::Object(Default::default()));
    std::fs::remove_dir_all(&dir).ok();
  }
}
