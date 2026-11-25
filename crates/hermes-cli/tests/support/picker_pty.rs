use super::{read_until, start_command_in_pty, terminal_flags};
use hermes_cli::{curses_checklist, curses_select, curses_select_embedded};
use std::collections::HashSet;
use std::io::Write;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

const PICKER_MODE: &str = "HERMES_TEST_NATIVE_PICKER_MODE";

#[test]
fn native_picker_driver() {
    let Ok(mode) = std::env::var(PICKER_MODE) else {
        return;
    };
    let choices: Vec<_> = (0..35).map(|i| format!("Choice {i:02}")).collect();
    let selected = match mode.as_str() {
        "standalone" => curses_select("Select a survey", &choices, 0),
        "embedded" => {
            crossterm::terminal::enable_raw_mode().unwrap();
            let result = curses_select_embedded("Select a report", &choices, 9);
            assert!(!hermes_cli::checklist::embedded_picker_active());
            crossterm::terminal::disable_raw_mode().unwrap();
            result
        }
        "checklist-confirm" | "checklist-cancel" => {
            let initial = HashSet::from([0, 2]);
            let status = |chosen: &HashSet<usize>| {
                if mode == "checklist-confirm" {
                    format!("Fixture chosen {}", chosen.len())
                } else {
                    String::new()
                }
            };
            let result =
                curses_checklist("Select survey records", &choices, &initial, Some(&status));
            let mut selected: Vec<_> = result.selected.into_iter().collect();
            selected.sort_unstable();
            println!(
                "CHECKLIST_RESULT selected={selected:?} confirmed={}",
                result.confirmed
            );
            return;
        }
        other => panic!("unexpected picker fixture mode {other}"),
    };
    println!(
        "PICKER_RESULT index={} confirmed={}",
        selected.index, selected.confirmed
    );
}

fn picker_command(sandbox: &std::path::Path, mode: &str) -> Command {
    let executable = std::env::current_dir()
        .unwrap()
        .join(std::env::args_os().next().expect("test executable"));
    let mut command = Command::new(executable);
    command
        .env_clear()
        .env("HOME", sandbox)
        .env("HERMES_HOME", sandbox.join("hermes"))
        .env(PICKER_MODE, mode)
        .current_dir(sandbox)
        .kill_on_drop(true)
        .args(["--exact", "picker_pty::native_picker_driver", "--nocapture"]);
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    command
}

#[tokio::test]
async fn standalone_picker_wraps_scrolls_and_confirms_the_highlighted_row() {
    let home = tempfile::tempdir().unwrap();
    let command = picker_command(home.path(), "standalone");
    let (mut child, mut terminal, initial_flags) = start_command_in_pty(command);
    read_until(&mut terminal, b"showing 1-27 of 35").await;

    terminal.write_all(b"k").unwrap();
    read_until(&mut terminal, b"showing 9-35 of 35").await;
    terminal.write_all(b"jjx\r").unwrap();
    let output = read_until(&mut terminal, b"PICKER_RESULT index=1 confirmed=true").await;
    assert!(output.windows(8).any(|bytes| bytes == b"\x1b[?1049l"));
    assert!(timeout(Duration::from_secs(5), child.wait())
        .await
        .unwrap()
        .unwrap()
        .success());
    assert_eq!(terminal_flags(&terminal), initial_flags);
}

#[tokio::test]
async fn embedded_picker_cancel_preserves_initial_selection_and_screen() {
    let home = tempfile::tempdir().unwrap();
    let command = picker_command(home.path(), "embedded");
    let (mut child, mut terminal, initial_flags) = start_command_in_pty(command);
    let initial = read_until(&mut terminal, b"showing 1-27 of 35").await;
    assert!(!initial.windows(8).any(|bytes| bytes == b"\x1b[?1049h"));

    terminal.write_all(b"j").unwrap();
    read_until(&mut terminal, "❯ Choice 10".as_bytes()).await;
    terminal.write_all(b"q").unwrap();
    let output = read_until(&mut terminal, b"PICKER_RESULT index=9 confirmed=false").await;
    assert!(!output.windows(8).any(|bytes| bytes == b"\x1b[?1049l"));
    assert!(timeout(Duration::from_secs(5), child.wait())
        .await
        .unwrap()
        .unwrap()
        .success());
    assert_eq!(terminal_flags(&terminal), initial_flags);
}

#[tokio::test]
async fn checklist_confirms_toggles_and_cancels_without_mutating_defaults() {
    let home = tempfile::tempdir().unwrap();
    for (mode, expected) in [
        (
            "checklist-confirm",
            "CHECKLIST_RESULT selected=[2, 34] confirmed=true",
        ),
        (
            "checklist-cancel",
            "CHECKLIST_RESULT selected=[0, 2] confirmed=false",
        ),
    ] {
        let command = picker_command(home.path(), mode);
        let (mut child, mut terminal, initial_flags) = start_command_in_pty(command);
        if mode == "checklist-confirm" {
            read_until(&mut terminal, b"Fixture chosen 2").await;
            terminal.write_all(b" ").unwrap();
            read_until(&mut terminal, b"Fixture chosen 1").await;
            terminal.write_all(b"k").unwrap();
            read_until(&mut terminal, b"items 10-35 of 35").await;
            terminal.write_all(b" \r").unwrap();
        } else {
            read_until(&mut terminal, b"items 1-26 of 35").await;
            terminal.write_all(b"j ").unwrap();
            read_until(&mut terminal, "❯ [✓] Choice 01".as_bytes()).await;
            terminal.write_all(b"q").unwrap();
        }
        let output = read_until(&mut terminal, expected.as_bytes()).await;
        assert!(output.windows(8).any(|bytes| bytes == b"\x1b[?1049l"));
        assert!(timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success());
        assert_eq!(terminal_flags(&terminal), initial_flags);
    }
}
