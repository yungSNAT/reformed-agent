#[cfg(unix)]
use super::*;

#[cfg(unix)]
#[tokio::test]
async fn run_quick_exec_returns_stderr_when_stdout_is_empty() {
    let output = run_quick_exec("stderr_only", "echo quick-stderr-message 1>&2", 5)
        .await
        .expect("quick exec should succeed");
    assert_eq!(output, "quick-stderr-message");
}

#[cfg(unix)]
#[tokio::test]
async fn run_quick_exec_trims_trailing_whitespace_from_stderr() {
    let output = run_quick_exec(
        "stderr_trailing",
        "printf 'line-one\\nline-two\\n\\n' 1>&2",
        5,
    )
    .await
    .expect("quick exec should succeed");
    assert_eq!(output, "line-one\nline-two");
}

#[cfg(unix)]
#[tokio::test]
async fn run_quick_exec_prefers_stdout_over_stderr() {
    let output = run_quick_exec("both_streams", "echo out-text; echo err-text 1>&2", 5)
        .await
        .expect("quick exec should succeed");
    assert_eq!(output, "out-text");
}

#[cfg(unix)]
#[tokio::test]
async fn run_quick_exec_reports_no_output() {
    let output = run_quick_exec("silent", "true", 5)
        .await
        .expect("quick exec should succeed");
    assert_eq!(output, "Quick command completed with no output.");
}
