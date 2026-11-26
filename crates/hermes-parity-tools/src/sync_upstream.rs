//! Port of `scripts/sync-upstream.sh` (stub).

use clap::Args;

#[derive(Args, Debug)]
#[command(disable_help_flag = true)]
pub struct SyncUpstreamArgs {
    /// Options as `scripts/sync-upstream.sh` takes them (see `--help`)
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

pub fn run(_args: SyncUpstreamArgs) -> Result<i32, String> {
    Ok(99)
}
