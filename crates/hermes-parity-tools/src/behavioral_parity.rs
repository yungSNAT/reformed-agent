//! Behavior-level parity contract for Hermes Ultra command + TUI surfaces.
//! Port of `scripts/run-behavioral-parity-suite.py`.

use std::path::{Path, PathBuf};

use clap::Args;

use crate::py::{self, Py};
use crate::report_io::resolve_argument;
use crate::script_helpers::{read_rust_surface, write_text_creating_parents};

const REQUIRED_COMMAND_TOKENS: &[&str] = &[
    "/plan caps",
    "/autocompact governance",
    "/model failover",
    "/ops cockpit",
    "/skills quality",
    "/raw trace focus",
    "/raw trace graph",
];

const REQUIRED_TUI_TOKENS: &[&str] = &["ActivityLaneMode::Cockpit", "Ctrl+O cockpit"];

#[derive(Args, Debug)]
pub struct BehavioralParityArgs {
    /// Repository root
    #[arg(long, default_value = ".")]
    repo_root: String,
    /// Optional report path (default: .sync-reports/behavioral-parity-*.json)
    #[arg(long, default_value = "")]
    report: String,
}

fn missing_tokens<'a>(raw: &str, tokens: &[&'a str]) -> Vec<&'a str> {
    tokens
        .iter()
        .copied()
        .filter(|token| !raw.contains(*token))
        .collect()
}

pub fn run(args: BehavioralParityArgs) -> Result<i32, String> {
    let repo_root = resolve_argument(Path::new(&args.repo_root))?;
    let commands_raw = read_rust_surface(&repo_root.join("crates/hermes-cli/src/commands"))?;
    let tui_raw = read_rust_surface(&repo_root.join("crates/hermes-cli/src/tui.rs"))?;

    let missing_cmd = missing_tokens(&commands_raw, REQUIRED_COMMAND_TOKENS);
    let missing_tui = missing_tokens(&tui_raw, REQUIRED_TUI_TOKENS);
    let ok = missing_cmd.is_empty() && missing_tui.is_empty();

    let payload = Py::dict([
        ("generated_at", Py::from(py::utc_isoformat())),
        ("repo_root", Py::from(py::path_str(&repo_root))),
        ("required_command_tokens", Py::strs(REQUIRED_COMMAND_TOKENS)),
        ("required_tui_tokens", Py::strs(REQUIRED_TUI_TOKENS)),
        ("missing_command_tokens", Py::strs(&missing_cmd)),
        ("missing_tui_tokens", Py::strs(&missing_tui)),
        ("ok", Py::from(ok)),
    ]);

    let report_path: PathBuf = if args.report.is_empty() {
        repo_root
            .join(".sync-reports")
            .join(format!("behavioral-parity-{}.json", py::utc_stamp()))
    } else {
        resolve_argument(Path::new(&args.report))?
    };

    write_text_creating_parents(&report_path, &py::dumps(&payload, Some(2), false))?;

    let status = if ok { "PASS" } else { "FAIL" };
    println!("[behavioral-parity] {status}");
    println!("[behavioral-parity] Report: {}", py::path_str(&report_path));
    if !missing_cmd.is_empty() {
        println!("[behavioral-parity] Missing command tokens:");
        for token in &missing_cmd {
            println!("  - {token}");
        }
    }
    if !missing_tui.is_empty() {
        println!("[behavioral-parity] Missing TUI tokens:");
        for token in &missing_tui {
            println!("  - {token}");
        }
    }

    Ok(i32::from(!ok))
}
