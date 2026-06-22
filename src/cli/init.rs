//! `llamastash init` CLI handler. Thin shim into `init::wizard::run`
//! so the wizard's body can evolve without touching the dispatcher
//! again.

use crate::cli::cli_args::{Cli, InitArgs};
use crate::cli::exit_codes::CliResult;
use crate::config::Config;

pub async fn handle(args: InitArgs, cli: &Cli, config: &Config) -> CliResult {
  // Collapse an `init <step>` subcommand into the flat `--only` shape
  // before the wizard runs, so the orchestration stays subcommand-blind.
  crate::init::wizard::run(args.fold_step(), cli, config).await
}
