//! `llamastash pull <hf-repo>` — MVP for the HF pull primitive. Thin
//! shim into `init::download::run`, which owns the multi-shard
//! download body.

use crate::cli::cli_args::{Cli, PullArgs};
use crate::cli::exit_codes::CliResult;
use crate::config::Config;

pub async fn handle(args: PullArgs, cli: &Cli, config: &Config) -> CliResult {
  crate::init::download::run(args, cli, config).await
}
