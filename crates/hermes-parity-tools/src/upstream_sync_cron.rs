//! Port of `scripts/cron-upstream-sync.sh` and
//! `scripts/install-upstream-sync-cron.sh` (stub).

use clap::Args;

#[derive(Args, Debug)]
pub struct CronArgs {}

#[derive(Args, Debug)]
pub struct InstallArgs {
    /// Cron schedule (default: `17 */6 * * *`); later arguments are ignored
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

pub fn run_cron(_args: CronArgs) -> Result<i32, String> {
    Ok(99)
}

pub fn run_install(_args: InstallArgs) -> Result<i32, String> {
    Ok(99)
}
