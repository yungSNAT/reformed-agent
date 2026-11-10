//! A foreground command must not read the agent's own stdin, which under ACP
//! or `mcp serve` carries the JSON-RPC stream. This file holds one test so it
//! can replace the test process's stdin without affecting other tests.
#![cfg(unix)]

use hermes_core::TerminalBackend;
use hermes_environments::LocalBackend;

#[tokio::test]
async fn foreground_command_does_not_read_the_agents_stdin() {
    let mut fds = [0; 2];
    // SAFETY: plain POSIX calls on descriptors this test owns; this test binary
    // has no other test that uses stdin.
    unsafe {
        assert_eq!(libc::pipe(fds.as_mut_ptr()), 0);
        let canary = b"AGENT-STDIN-CANARY\n";
        assert_eq!(
            libc::write(fds[1], canary.as_ptr().cast(), canary.len()),
            canary.len() as isize
        );
        assert_eq!(libc::close(fds[1]), 0);
        assert_eq!(libc::dup2(fds[0], 0), 0);
        assert_eq!(libc::close(fds[0]), 0);
    }

    let backend = LocalBackend::default();
    let output = backend
        .execute_command("cat", Some(10), None, false, false)
        .await
        .unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(
        !output.stdout.contains("AGENT-STDIN-CANARY"),
        "foreground command read the agent's stdin: {:?}",
        output.stdout
    );
}
