//! Print comparison cases for `fixtures/`, recorded from a Hermes Python checkout at
//! `../hermes-agent` beside this repository when one exists. See `hermes_parity_tests::recorder`.

use std::io::Write;
use std::path::Path;

fn main() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate sits two levels below the repository root");
    let py_root = repo_root.parent().unwrap_or(repo_root).join("hermes-agent");
    let python = std::env::var("PARITY_PYTHON").unwrap_or_else(|_| "python3".into());
    let recorded = hermes_parity_tests::recorder::record(&py_root, &python);
    std::io::stdout()
        .write_all(recorded.stdout.as_bytes())
        .expect("write stdout");
    std::io::stderr()
        .write_all(recorded.stderr.as_bytes())
        .expect("write stderr");
    std::process::exit(recorded.code);
}
