//! `llamastash list` — enumerate discovered models.
//!
//! TSV-by-default output keeps it pipe-friendly; `--json` emits the
//! stable agent-facing array. `--filter` is a substring matched
//! against name, path, arch, and quant (mirrors the TUI's `/`).

use crate::cli::cli_args::{Cli, ListArgs};
use crate::cli::client::connect_or_spawn;
use crate::cli::exit_codes::CliResult;
use crate::cli::output::{filter_rows, list_human, list_json, pretty_json};
use crate::cli::resolve::{fetch_catalog, fetch_status, running_index};
use crate::config::Config;

pub async fn handle(args: ListArgs, cli: &Cli, config: &Config) -> CliResult {
  let mut client = connect_or_spawn(cli, config).await?;
  let mut rows = fetch_catalog(&mut client).await?;
  if let Some(pat) = &args.filter {
    rows = filter_rows(&rows, pat);
  }
  // Augment with running-state info so STATUS / port land alongside
  // the catalog row. Best-effort: a daemon that fails to answer
  // `status` is treated as "nothing running" rather than erroring out
  // the list itself.
  let running = match fetch_status(&mut client).await {
    Ok(snap) => running_index(&snap.models),
    Err(_) => Default::default(),
  };
  if args.json {
    println!("{}", pretty_json(&list_json(&rows, &running)));
  } else {
    print!("{}", list_human(&rows, &running));
  }
  Ok(())
}
