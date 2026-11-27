//! Print comparison cases for `fixtures/`, recorded from a Hermes Python checkout at
//! `../hermes-agent` beside this repository when one exists. See `hermes_parity_tests::recorder`.

use std::io::{IsTerminal, Write};
use std::path::Path;

use hermes_parity_tests::recorder;

fn write(mut stream: impl Write + IsTerminal, bytes: &[u8]) -> std::io::Result<()> {
    let console = cfg!(windows) && stream.is_terminal();
    stream.write_all(&recorder::for_stream(bytes, console))
}

fn main() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate sits two levels below the repository root");
    let py_root = recorder::python_checkout_beside(repo_root);
    let consoles: Vec<&str> = [
        ("stdout", std::io::stdout().is_terminal()),
        ("stderr", std::io::stderr().is_terminal()),
    ]
    .into_iter()
    .filter(|&(_, terminal)| cfg!(windows) && terminal)
    .map(|(name, _)| name)
    .collect();
    let recorded = recorder::record_for_consoles(&py_root, &recorder::python_from_env(), &consoles);
    // A closed pipe (`| head`) ends the run with 1, as the script's BrokenPipeError did;
    // the adapter's stderr is still written.
    let out = write(std::io::stdout(), &recorded.stdout);
    let err = write(std::io::stderr(), &recorded.stderr);
    std::process::exit(if out.is_ok() && err.is_ok() {
        recorded.code
    } else {
        1
    });
}
