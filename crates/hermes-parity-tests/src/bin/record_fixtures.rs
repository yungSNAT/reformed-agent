//! Print comparison cases for `fixtures/`, recorded from a Hermes Python checkout at
//! `../hermes-agent` beside this repository when one exists. See `hermes_parity_tests::recorder`.

use std::io::Write;
use std::path::Path;

use hermes_parity_tests::recorder;

fn main() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate sits two levels below the repository root");
    let py_root = recorder::python_checkout_beside(repo_root);
    let recorded = recorder::record(&py_root, &recorder::python_from_env());
    // A closed pipe (`| head`) ends the run with 1, as the script's BrokenPipeError did.
    let written = std::io::stdout()
        .write_all(native_newlines(&recorded.stdout).as_bytes())
        .and_then(|()| std::io::stderr().write_all(native_newlines(&recorded.stderr).as_bytes()));
    std::process::exit(if written.is_ok() { recorded.code } else { 1 });
}

/// Python's text streams end lines with CRLF on Windows; match them byte for byte there.
fn native_newlines(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    if cfg!(windows) {
        text.replace('\n', "\r\n")
    } else {
        text
    }
}
