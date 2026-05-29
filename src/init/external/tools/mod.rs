//! Per-tool patcher modules. Each module declares one
//! [`super::ToolPatcher`]; [`registered`] returns the full list in
//! picker display order.

pub mod aider;
pub mod continue_dev;
pub mod env_sh;
pub mod opencode;
pub mod pi_dev;
pub mod zed;

use super::ToolPatcher;

/// Every registered patcher, in picker display order. Order matters:
/// it's what `cliclack::multiselect` presents to the user. The
/// `env-sh` writer is last so the user sees it as the "and also set
/// shell envs" affordance after the per-tool entries.
pub fn registered() -> Vec<Box<dyn ToolPatcher>> {
  vec![
    Box::new(opencode::OpenCode),
    Box::new(aider::Aider),
    Box::new(continue_dev::ContinueDev),
    Box::new(zed::Zed),
    Box::new(pi_dev::PiDev),
    Box::new(env_sh::EnvSh),
  ]
}
