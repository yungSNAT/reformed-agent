#![cfg(target_os = "linux")]

#[path = "support/isolated_hermes.rs"]
mod support;

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::Path;
use std::time::Duration;
use tokio::process::Child;
use tokio::time::{sleep, timeout};

fn terminal_flags(terminal: &File) -> libc::tcflag_t {
    let mut attributes = std::mem::MaybeUninit::<libc::termios>::uninit();
    assert_eq!(
        unsafe { libc::tcgetattr(terminal.as_raw_fd(), attributes.as_mut_ptr()) },
        0,
        "{}",
        io::Error::last_os_error()
    );
    // tcgetattr initialises the termios when it succeeds.
    unsafe { attributes.assume_init().c_lflag }
}

fn start_tui(sandbox: &Path) -> (Child, File, libc::tcflag_t) {
    let mut master_fd = -1;
    let mut slave_fd = -1;
    let dimensions = libc::winsize {
        ws_row: 32,
        ws_col: 120,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // openpty fills both descriptors on success. These owned files close them.
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null(),
                &dimensions,
            )
        },
        0,
        "{}",
        io::Error::last_os_error()
    );
    let (master, slave) = unsafe { (File::from_raw_fd(master_fd), File::from_raw_fd(slave_fd)) };
    for descriptor in [master.as_raw_fd(), slave.as_raw_fd()] {
        assert_eq!(
            unsafe { libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC) },
            0,
            "{}",
            io::Error::last_os_error()
        );
    }
    let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
    assert_ne!(flags, -1, "{}", io::Error::last_os_error());
    assert_eq!(
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0,
        "{}",
        io::Error::last_os_error()
    );

    let initial_flags = terminal_flags(&slave);
    let mut command = support::isolated_hermes(sandbox);
    command
        .env("TERM", "xterm-256color")
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .args(["--ignore-user-config", "--ignore-rules"])
        .stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave);
    // The child attaches only its own PTY slave so crossterm can open /dev/tty.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 || libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().expect("start isolated CLI in PTY");
    drop(command);
    (child, master, initial_flags)
}

async fn read_until(terminal: &mut File, expected: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    let result = timeout(Duration::from_secs(12), async {
        let mut chunk = [0; 4096];
        loop {
            match terminal.read(&mut chunk) {
                Ok(0) => return false,
                Ok(read) => {
                    output.extend_from_slice(&chunk[..read]);
                    if output
                        .windows(expected.len())
                        .any(|window| window == expected)
                    {
                        return true;
                    }
                    if output.len() > 128 * 1024 {
                        output.drain(..output.len() - 64 * 1024);
                    }
                    tokio::task::yield_now().await;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    sleep(Duration::from_millis(10)).await;
                }
                // Linux reports EIO on the PTY master after its final slave closes.
                Err(error) if error.raw_os_error() == Some(libc::EIO) => return false,
                Err(error) => panic!("read the isolated PTY: {error}"),
            }
        }
    })
    .await;
    let tail = &output[output.len().saturating_sub(1400)..];
    assert!(
        matches!(result, Ok(true)),
        "missing {:?} at end of PTY output: {:?}",
        String::from_utf8_lossy(expected),
        String::from_utf8_lossy(tail)
    );
    output
}

#[tokio::test]
async fn native_tui_handles_pasted_commands_and_restores_the_terminal_on_interrupt() {
    let sandbox = tempfile::tempdir().unwrap();
    let (mut child, mut terminal, initial_flags) = start_tui(sandbox.path());
    read_until(&mut terminal, b"\x1b[?1049h").await;
    terminal
        .write_all(b"\x1b[200~/objective Check the synthetic fixture\x1b[201~")
        .unwrap();
    read_until(&mut terminal, b"Pasted").await;
    terminal.write_all(b"\r").unwrap();
    let contract = sandbox.path().join("hermes/alpha/objective_contract.json");
    let objective = timeout(Duration::from_secs(12), async {
        loop {
            if let Ok(bytes) = std::fs::read(&contract) {
                if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    break value;
                }
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("pasted objective command produces a contract");
    assert_eq!(objective["objective_text"], "Check the synthetic fixture");
    terminal.write_all(b"\x03").unwrap();
    let restored = read_until(&mut terminal, b"\x1b[?1049l").await;
    assert!(restored.windows(8).any(|bytes| bytes == b"\x1b[?2004l"));

    assert!(timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("TUI shutdown timeout")
        .expect("wait for isolated CLI")
        .success());
    assert_eq!(terminal_flags(&terminal), initial_flags);
    assert!(!sandbox
        .path()
        .join("hermes/interactive.session.lock")
        .exists());
}
