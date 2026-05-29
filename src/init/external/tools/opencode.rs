//! OpenCode patcher — `~/.config/opencode/opencode.json`.
//!
//! Registers a `llamastash` provider via the
//! `@ai-sdk/openai-compatible` SDK package and points it at the
//! local llamastash proxy. Per OpenCode's docs, custom providers
//! live under `provider.<id>` with `npm`, `name`, `options.baseURL`,
//! and a `models` map.
//!
//! API key: rendered as the `{env:LLAMASTASH_API_KEY}` reference so
//! the literal value never lands on disk — the env-var hop costs
//! nothing because llama-server ignores Authorization anyway.

use std::path::PathBuf;

use serde_json::json;

use crate::init::external::{Format, PatchContext, ToolPatcher};

pub struct OpenCode;

impl ToolPatcher for OpenCode {
  fn id(&self) -> &'static str {
    "opencode"
  }
  fn display_name(&self) -> &'static str {
    "OpenCode"
  }
  fn default_path(&self) -> Option<PathBuf> {
    crate::util::paths::home_dir().map(|h| h.join(".config").join("opencode").join("opencode.json"))
  }
  fn format(&self) -> Format {
    Format::Json
  }
  fn build_additions(&self, ctx: &PatchContext) -> serde_json::Value {
    let mut models = serde_json::Map::new();
    if let Some(id) = &ctx.model_id {
      models.insert(id.clone(), json!({ "name": id }));
    }
    json!({
      "$schema": "https://opencode.ai/config.json",
      "provider": {
        "llamastash": {
          "npm": "@ai-sdk/openai-compatible",
          "name": "LlamaStash",
          "options": {
            "baseURL": ctx.proxy_base_url,
          },
          "apiKey": "{env:LLAMASTASH_API_KEY}",
          "models": models,
        }
      }
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::init::external::{apply, dry_run};

  fn ctx() -> PatchContext {
    PatchContext {
      proxy_base_url: "http://127.0.0.1:11435/v1".into(),
      api_key: "llamastash".into(),
      model_id: Some("qwen3-coder-30b".into()),
    }
  }

  #[test]
  fn writes_provider_block_into_empty_file() {
    let dir = crate::util::test_temp::unique_temp_dir("opencode-empty");
    let path = dir.join("opencode.json");
    let out = apply(&OpenCode, &ctx(), Some(path.clone())).expect("apply");
    assert!(out.written_bytes > 0);
    let body: serde_json::Value =
      serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
      body["provider"]["llamastash"]["npm"],
      "@ai-sdk/openai-compatible"
    );
    assert_eq!(
      body["provider"]["llamastash"]["options"]["baseURL"],
      "http://127.0.0.1:11435/v1"
    );
    assert_eq!(
      body["provider"]["llamastash"]["models"]["qwen3-coder-30b"]["name"],
      "qwen3-coder-30b"
    );
    std::fs::remove_dir_all(&dir).ok();
  }

  #[test]
  fn preserves_user_providers_alongside_llamastash() {
    let dir = crate::util::test_temp::unique_temp_dir("opencode-coexist");
    let path = dir.join("opencode.json");
    std::fs::write(&path, r#"{"provider":{"anthropic":{"name":"Anthropic"}}}"#).unwrap();
    apply(&OpenCode, &ctx(), Some(path.clone())).expect("apply");
    let body: serde_json::Value =
      serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(body["provider"]["anthropic"]["name"], "Anthropic");
    assert!(body["provider"]["llamastash"].is_object());
    std::fs::remove_dir_all(&dir).ok();
  }

  #[test]
  fn idempotent_apply_produces_no_second_diff() {
    let dir = crate::util::test_temp::unique_temp_dir("opencode-idem");
    let path = dir.join("opencode.json");
    apply(&OpenCode, &ctx(), Some(path.clone())).expect("first");
    let second = apply(&OpenCode, &ctx(), Some(path.clone())).expect("second");
    assert!(second.diff_json.is_empty());
    std::fs::remove_dir_all(&dir).ok();
  }

  #[test]
  fn api_key_renders_as_env_reference_not_literal() {
    let ctx = ctx();
    let v = OpenCode.build_additions(&ctx);
    assert_eq!(
      v["provider"]["llamastash"]["apiKey"],
      "{env:LLAMASTASH_API_KEY}"
    );
  }

  #[test]
  fn dry_run_reports_baseurl_change_for_existing_install() {
    let dir = crate::util::test_temp::unique_temp_dir("opencode-dry");
    let path = dir.join("opencode.json");
    std::fs::write(
      &path,
      r#"{"provider":{"llamastash":{"npm":"@ai-sdk/openai-compatible","name":"LlamaStash","options":{"baseURL":"http://127.0.0.1:99999/v1"},"apiKey":"{env:LLAMASTASH_API_KEY}","models":{"qwen3-coder-30b":{"name":"qwen3-coder-30b"}}}}}"#,
    )
    .unwrap();
    let out = dry_run(&OpenCode, &ctx(), Some(path)).expect("dry_run");
    let leaf = out
      .diff_json
      .iter()
      .find(|d| d.path == "provider.llamastash.options.baseURL")
      .expect("baseURL leaf");
    assert_eq!(leaf.kind, "changed");
    std::fs::remove_dir_all(&dir).ok();
  }
}
