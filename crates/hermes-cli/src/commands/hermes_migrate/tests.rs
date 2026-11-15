//! Fixture Hermes homes with fake secrets only.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use hermes_cron::{CronJob, DeliverTarget, JobStatus};
use rusqlite::{params, Connection};
use serde_json::{json, Value};

use super::{cron, migrate_hermes, MigrateReport, MigrateRequest};
use crate::commands::import_hermes_sessions;

const SECRET: &str = "fake-openrouter-key-0123456789";
const TOKEN: &str = "fake-bot-token-9876543210";
const EXPORT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../hermes-parity-tests/fixtures/session_import/hermes_export.jsonl"
));
const FIRST: &str = "20261109_103000_c0ffeed0";
const SECOND: &str = "20261109_103200_c0ffeed1";

/// Every file under `root` with its bytes, links and sidecars included.
fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                pending.push(path);
            } else {
                let bytes = if meta.is_file() {
                    fs::read(&path).unwrap()
                } else {
                    Vec::new()
                };
                files.insert(path.strip_prefix(root).unwrap().to_path_buf(), bytes);
            }
        }
    }
    files
}

/// The JSONL fixture's 2 sessions as Hermes stores them, with the first
/// prompt behind `system_prompts` and a JSON-encoded content row.
fn state_db(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         CREATE TABLE sessions (id TEXT PRIMARY KEY, source TEXT NOT NULL, user_id TEXT,
           model TEXT, model_config TEXT, system_prompt TEXT, system_prompt_hash TEXT,
           parent_session_id TEXT, started_at REAL NOT NULL, ended_at REAL, end_reason TEXT,
           title TEXT, last_activity_at REAL);
         CREATE TABLE system_prompts (hash TEXT PRIMARY KEY, prompt TEXT NOT NULL);
         CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL,
           role TEXT NOT NULL, content TEXT, tool_call_id TEXT, tool_calls TEXT, tool_name TEXT,
           timestamp REAL NOT NULL, reasoning_content TEXT, active INTEGER NOT NULL DEFAULT 1,
           display_identity BLOB);
         INSERT INTO system_prompts VALUES ('h1', 'Synthetic test session.');",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sessions (id, source, model, model_config, system_prompt_hash, started_at,
           ended_at, end_reason, title) VALUES (?1, 'cli', 'openai:gpt-4o-mini', ?2, 'h1',
           1794220200.125, 1794220260.5, 'compression', 'Croft Manor notes')",
        params![FIRST, r#"{"reasoning_effort":"medium"}"#],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sessions (id, source, model, parent_session_id, started_at, title,
           last_activity_at) VALUES (?1, 'cli', 'openai:gpt-4o-mini', ?2, 1794220320,
           'Second page', 1794220360)",
        params![SECOND, FIRST],
    )
    .unwrap();
    let calls = r#"[{"id":"call_fixture","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"fixture.txt\"}"}}]"#;
    let rows = [
        (
            FIRST,
            "user",
            Some("\u{0}json:\"Keep the notes in order.\""),
            None,
            None,
            None,
            1794220201.0,
            1,
        ),
        (
            FIRST,
            "assistant",
            None,
            Some(calls),
            None,
            Some("Read the fixture."),
            1794220202.0,
            1,
        ),
        (
            FIRST,
            "tool",
            Some("The notes are ready."),
            None,
            Some("call_fixture"),
            None,
            1794220203.0,
            1,
        ),
        (
            FIRST,
            "assistant",
            Some("Removed turn."),
            None,
            None,
            None,
            1794220204.0,
            0,
        ),
        (
            SECOND,
            "session_meta",
            Some("Internal metadata."),
            None,
            None,
            None,
            1794220321.0,
            1,
        ),
        (
            SECOND,
            "user",
            Some("Continue the notes."),
            None,
            None,
            None,
            1794220322.0,
            1,
        ),
        (
            SECOND,
            "assistant",
            Some("Here is the second page."),
            None,
            None,
            None,
            1794220370.5,
            1,
        ),
    ];
    for (session, role, content, tool_calls, call_id, reasoning, time, active) in rows {
        let tool_name = call_id.map(|_| "read_file");
        conn.execute(
            "INSERT INTO messages (session_id, role, content, tool_calls, tool_call_id, tool_name,
               reasoning_content, timestamp, active, display_identity)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, x'00ff')",
            params![
                session, role, content, tool_calls, call_id, tool_name, reasoning, time, active
            ],
        )
        .unwrap();
    }
}

fn jobs_json() -> Value {
    json!({"jobs": [
        {"id": "a1b2c3", "name": format!("token {TOKEN}"), "prompt": format!("use {SECRET}"),
         "schedule": {"kind": "cron", "expr": "0 9 * * 1"}, "deliver": "local",
         "repeat": {"times": null, "completed": 0}, "created_at": "2026-11-01T09:00:00+11:00"},
        {"id": "d4e5f6", "prompt": "check", "script": "check.sh",
         "schedule": {"kind": "interval", "minutes": 30},
         "next_run_at": "2026-11-15T20:37:00+11:00"},
        {"id": "CON", "prompt": "x", "schedule": {"kind": "cron", "expr": "* * * * *"}},
    ]})
}

fn hermes_home(root: &Path) -> PathBuf {
    let home = root.join("hermes");
    for dir in [
        "memories",
        "skills/notes",
        "plugins/echo",
        "scripts",
        "cron",
    ] {
        fs::create_dir_all(home.join(dir)).unwrap();
    }
    let files = [
        (
            "config.yaml",
            format!("model:\n  default: openai/gpt-4o-mini\n  api_key: {SECRET}\n"),
        ),
        (
            ".env",
            format!("OPENROUTER_API_KEY={SECRET}\nTELEGRAM_BOT_TOKEN={TOKEN}\n"),
        ),
        ("SOUL.md", "Be brief.\n".into()),
        ("auth.json", format!("{{\"token\": \"{TOKEN}\"}}")),
        ("memories/MEMORY.md", "Prefers metric units.\n".into()),
        ("memories/USER.md", "Lives in Hobart.\n".into()),
        (
            "skills/notes/SKILL.md",
            "---\nname: notes\n---\nKeep notes.\n".into(),
        ),
        ("skills/.bundled_manifest", "notes:abc\n".into()),
        ("plugins/echo/plugin.yaml", "name: echo\n".into()),
        ("scripts/check.sh", "#!/bin/sh\necho ok\n".into()),
        ("cron/jobs.json", jobs_json().to_string()),
    ];
    for (name, text) in files {
        fs::write(home.join(name), text).unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            home.join("scripts/check.sh"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    state_db(&home.join("state.db"));
    home
}

fn run(from: &Path, to: &Path, apply: bool, into_existing: bool) -> MigrateReport {
    migrate_hermes(&MigrateRequest {
        from,
        to,
        apply,
        into_existing,
    })
    .unwrap()
}

fn assert_no_secret(report: &MigrateReport) {
    let text = report.lines.join("\n");
    for secret in [SECRET, TOKEN] {
        assert!(!text.contains(secret), "{text}");
    }
}

#[test]
fn a_dry_run_writes_nothing_and_prints_no_secret() {
    let root = tempfile::tempdir().unwrap();
    let from = hermes_home(root.path());
    let before = tree(&from);
    let to = root.path().join("reformed");

    let report = run(&from, &to, false, false);

    assert!(!report.failed, "{:#?}", report.lines);
    assert!(!to.exists());
    assert_eq!(tree(&from), before, "the Hermes home changed");
    assert_no_secret(&report);
    let text = report.lines.join("\n");
    for line in [
        ".env: to copy",
        "skills/: to copy, 2 files",
        "state.db: to copy 2 sessions with 5 active messages; 2 inactive or metadata rows left out",
        "cron/jobs.json: 2 jobs to write paused, 1 skipped",
        "  job a1b2c3: 0 9 * * 1, now read in UTC",
        "  job d4e5f6: every 30 minutes, as 7,37 * * * * in UTC",
        "  job #3: skipped, its id is not a safe file name",
        "auth.json: not migrated",
        "Dry run: nothing was written.",
    ] {
        assert!(text.contains(line), "missing {line:?} in\n{text}");
    }
}

#[test]
fn apply_moves_every_item_privately() {
    let root = tempfile::tempdir().unwrap();
    let from = hermes_home(root.path());
    let before = tree(&from);
    let to = root.path().join("reformed");

    let report = run(&from, &to, true, false);

    assert!(!report.failed, "{:#?}", report.lines);
    assert_no_secret(&report);
    assert_eq!(tree(&from), before, "the Hermes home changed");
    for name in [
        "config.yaml",
        ".env",
        "SOUL.md",
        "memories/MEMORY.md",
        "memories/USER.md",
        "skills/notes/SKILL.md",
        "skills/.bundled_manifest",
        "plugins/echo/plugin.yaml",
        "scripts/check.sh",
    ] {
        assert_eq!(
            fs::read(to.join(name)).unwrap(),
            fs::read(from.join(name)).unwrap(),
            "{name}"
        );
    }
    assert!(!to.join("auth.json").exists());
    assert!(to.join("sessions.db").is_file());
    let job: CronJob =
        serde_json::from_slice(&fs::read(to.join("cron/a1b2c3.json")).unwrap()).unwrap();
    assert_eq!(job.status, JobStatus::Paused);
    assert_eq!(job.schedule, "0 9 * * 1");
    assert_eq!(job.prompt, format!("use {SECRET}"));
    let job: CronJob =
        serde_json::from_slice(&fs::read(to.join("cron/d4e5f6.json")).unwrap()).unwrap();
    assert_eq!(job.script.as_deref(), Some("./check.sh"));
    assert!(!to.join("cron/CON.json").exists());
    let leftovers: Vec<_> = fs::read_dir(to.as_path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().starts_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &str| fs::metadata(to.join(path)).unwrap().permissions().mode() & 0o777;
        for (path, expected) in [
            ("", 0o700),
            ("skills", 0o700),
            (".env", 0o600),
            ("config.yaml", 0o600),
            ("memories/MEMORY.md", 0o600),
            ("scripts/check.sh", 0o700),
            ("sessions.db", 0o600),
            ("sessions", 0o700),
            ("cron/a1b2c3.json", 0o600),
        ] {
            assert_eq!(mode(path), expected, "{path}");
        }
    }
}

#[test]
fn state_db_sessions_match_the_jsonl_import() {
    let root = tempfile::tempdir().unwrap();
    let from = hermes_home(root.path());
    let to = root.path().join("reformed");
    let export = root.path().join("export.jsonl");
    fs::write(&export, EXPORT).unwrap();
    let imported = root.path().join("imported");

    assert!(!run(&from, &to, true, false).failed);
    let summary = import_hermes_sessions(&export, &imported).unwrap();

    assert_eq!((summary.sessions, summary.messages), (2, 5));
    for id in [FIRST, SECOND] {
        let name = format!("sessions/{id}.json");
        assert_eq!(
            fs::read_to_string(to.join(&name)).unwrap(),
            fs::read_to_string(imported.join(&name)).unwrap(),
            "{id}"
        );
        assert_eq!(
            fs::metadata(to.join(&name)).unwrap().modified().unwrap(),
            fs::metadata(imported.join(&name))
                .unwrap()
                .modified()
                .unwrap(),
            "{id}"
        );
    }
    let rows = |home: &Path| -> Vec<(String, String, String)> {
        let conn = Connection::open(home.join("sessions.db")).unwrap();
        let mut statement = conn
            .prepare("SELECT id, created_at, updated_at FROM sessions ORDER BY id")
            .unwrap();
        let mapped = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap();
        mapped.collect::<Result<_, _>>().unwrap()
    };
    assert_eq!(rows(&to), rows(&imported));
}

#[test]
fn an_older_state_db_without_prompts_or_optional_columns_reads() {
    let root = tempfile::tempdir().unwrap();
    let from = root.path().join("hermes");
    fs::create_dir(&from).unwrap();
    let conn = Connection::open(from.join("state.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (id TEXT PRIMARY KEY, source TEXT, started_at REAL NOT NULL,
           system_prompt TEXT);
         CREATE TABLE messages (id INTEGER PRIMARY KEY, session_id TEXT, role TEXT,
           content TEXT, timestamp REAL);
         INSERT INTO sessions VALUES ('old_1', 'cli', 1794220200, 'Old prompt.');
         INSERT INTO messages VALUES (1, 'old_1', 'user', 'Hello.', 1794220201);
         INSERT INTO messages VALUES (2, 'gone', 'user', 'Orphan.', 1794220202);",
    )
    .unwrap();
    drop(conn);

    let to = root.path().join("reformed");
    let report = run(&from, &to, true, false);

    assert!(!report.failed, "{:#?}", report.lines);
    let text = report.lines.join("\n");
    assert!(
        text.contains("copied 1 sessions with 1 active messages"),
        "{text}"
    );
    assert!(
        text.contains("1 message rows belong to no session"),
        "{text}"
    );
    let snapshot: Value =
        serde_json::from_slice(&fs::read(to.join("sessions/old_1.json")).unwrap()).unwrap();
    assert_eq!(snapshot["system_prompt"], "Old prompt.");
}

#[test]
fn an_unrepresentable_session_fails_the_plan_and_apply_writes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let from = hermes_home(root.path());
    let conn = Connection::open(from.join("state.db")).unwrap();
    conn.execute(
        "INSERT INTO messages (session_id, role, content, tool_call_id, timestamp)
         VALUES (?1, 'tool', 'orphan result', 'call_missing', 1794220380)",
        params![SECOND],
    )
    .unwrap();
    drop(conn);
    let to = root.path().join("reformed");

    let report = run(&from, &to, true, false);

    assert!(report.failed);
    let text = report.lines.join("\n");
    assert!(
        text.contains(&format!("error: state.db: session {SECOND}:")),
        "{text}"
    );
    assert!(text.contains("Nothing was written"), "{text}");
    assert!(!to.exists());
}

#[test]
fn unapplied_wal_changes_without_shared_memory_are_refused() {
    let root = tempfile::tempdir().unwrap();
    let from = hermes_home(root.path());
    fs::write(from.join("state.db-wal"), b"not empty").unwrap();
    let to = root.path().join("reformed");

    let report = run(&from, &to, false, false);

    assert!(report.failed);
    assert!(report
        .lines
        .join("\n")
        .contains("start and stop Hermes once"));
}

#[test]
fn an_existing_home_is_refused_unless_asked_and_its_items_left_unchanged() {
    let root = tempfile::tempdir().unwrap();
    let from = hermes_home(root.path());
    let to = root.path().join("reformed");
    fs::create_dir_all(to.join("skills")).unwrap();
    fs::write(to.join(".env"), "MINE=1\n").unwrap();
    fs::create_dir(to.join("cron")).unwrap();
    fs::write(to.join("cron/a1b2c3.json"), "{}").unwrap();

    let refused = migrate_hermes(&MigrateRequest {
        from: &from,
        to: &to,
        apply: true,
        into_existing: false,
    })
    .unwrap_err();
    assert!(refused.to_string().contains("--into-existing"), "{refused}");

    let report = run(&from, &to, true, true);

    assert!(!report.failed, "{:#?}", report.lines);
    let text = report.lines.join("\n");
    assert!(
        text.contains(".env: skipped, already in the Reformed home"),
        "{text}"
    );
    assert!(
        text.contains("skills/: skipped, already in the Reformed home"),
        "{text}"
    );
    assert!(
        text.contains("job a1b2c3: skipped, already in the Reformed home"),
        "{text}"
    );
    assert_eq!(fs::read_to_string(to.join(".env")).unwrap(), "MINE=1\n");
    assert_eq!(
        fs::read_to_string(to.join("cron/a1b2c3.json")).unwrap(),
        "{}"
    );
    assert_eq!(fs::read_dir(to.join("skills")).unwrap().count(), 0);
    assert!(to.join("config.yaml").is_file());
    assert!(to.join("cron/d4e5f6.json").is_file());
}

#[test]
fn homes_inside_each_other_are_refused() {
    let root = tempfile::tempdir().unwrap();
    let from = hermes_home(root.path());
    for to in [from.join("reformed"), root.path().to_path_buf()] {
        let error = migrate_hermes(&MigrateRequest {
            from: &from,
            to: &to,
            apply: true,
            into_existing: true,
        })
        .unwrap_err();
        assert!(
            error.to_string().contains("neither inside the other"),
            "{error}"
        );
    }
    assert!(!from.join("reformed").exists());
}

#[cfg(unix)]
#[test]
fn links_are_neither_copied_nor_written_through() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let from = hermes_home(root.path());
    let outside = root.path().join("outside.txt");
    fs::write(&outside, SECRET).unwrap();
    symlink(&outside, from.join("skills/notes/leak.md")).unwrap();
    fs::remove_file(from.join("SOUL.md")).unwrap();
    symlink(outside, from.join("SOUL.md")).unwrap();
    let to = root.path().join("reformed");
    fs::create_dir(&to).unwrap();
    let elsewhere = root.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    symlink(&elsewhere, to.join("plugins")).unwrap();

    let report = run(&from, &to, true, true);

    assert!(!report.failed, "{:#?}", report.lines);
    let text = report.lines.join("\n");
    assert!(
        text.contains("skipped skills/notes/leak.md: a link or special file"),
        "{text}"
    );
    assert!(
        text.contains("SOUL.md: skipped, not a regular file"),
        "{text}"
    );
    assert!(
        text.contains("plugins/: skipped, already in the Reformed home"),
        "{text}"
    );
    assert!(!to.join("skills/notes/leak.md").exists());
    assert!(!to.join("SOUL.md").exists());
    assert_eq!(fs::read_dir(elsewhere).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn a_failed_apply_removes_what_it_created() {
    use std::os::unix::fs::PermissionsExt;
    // Root reads a mode-000 file, so the failure cannot be staged.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let from = hermes_home(root.path());
    let locked = from.join("skills/notes/SKILL.md");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let to = root.path().join("reformed");

    let error = migrate_hermes(&MigrateRequest {
        from: &from,
        to: &to,
        apply: true,
        into_existing: false,
    })
    .unwrap_err();

    fs::set_permissions(locked, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        error.to_string().contains("Migration write failed"),
        "{error}"
    );
    assert!(!to.exists(), "{:?}", tree(&to));
}

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-15T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn convert(job: Value) -> Result<CronJob, String> {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("check.sh"), "").unwrap();
    cron::convert(&job, now(), root.path()).map(|(job, _)| job)
}

#[test]
fn cron_schedules_convert_to_the_same_instants_in_utc() {
    for (schedule, extra, expected) in [
        (
            json!({"kind": "cron", "expr": "0 9 * * 1"}),
            json!({}),
            "0 9 * * 1",
        ),
        (
            json!({"kind": "cron", "expr": "15 10 * * 1-5 0"}),
            json!({}),
            "15 10 * * 1-5",
        ),
        (
            json!({"kind": "interval", "minutes": 30}),
            json!({"next_run_at": "2026-11-15T20:37:00+11:00"}),
            "7,37 * * * *",
        ),
        (
            json!({"kind": "interval", "minutes": 120}),
            json!({"last_run_at": "2026-11-15T03:15:00+00:00"}),
            "15 1,3,5,7,9,11,13,15,17,19,21,23 * * *",
        ),
        (
            json!({"kind": "interval", "minutes": 1440}),
            json!({"created_at": "2026-11-16T06:30:00.123456+00:00"}),
            "30 6 * * *",
        ),
        (
            json!({"kind": "once", "run_at": "2099-12-01T09:30:05+11:00"}),
            json!({}),
            "5 30 22 30 11 * 2099",
        ),
    ] {
        let mut job = json!({"id": "j1", "prompt": "p", "schedule": schedule.clone()});
        job.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let converted = convert(job).unwrap_or_else(|reason| panic!("{schedule}: {reason}"));
        assert_eq!(converted.schedule, expected);
        assert_eq!(converted.status, JobStatus::Paused);
        assert_eq!(
            converted.repeat,
            (schedule["kind"] == "once").then_some(1),
            "{schedule}"
        );
    }
}

#[test]
fn cron_fields_map_and_unsupported_jobs_are_skipped_with_a_reason() {
    let base =
        || json!({"id": "j1", "prompt": "p", "schedule": {"kind": "cron", "expr": "0 9 * * *"}});
    let with = |field: &str, value: Value| {
        let mut job = base();
        job[field] = value;
        job
    };
    let job = convert(json!({
        "id": "j1", "name": "n", "prompt": "p", "skills": ["notes"], "model": "m",
        "provider": "openrouter", "deliver": "origin",
        "origin": {"platform": "telegram", "chat_id": 42}, "script": "check.sh",
        "repeat": {"times": 5, "completed": 2}, "context_from": "j0",
        "schedule": {"kind": "cron", "expr": "0 9 * * *"}
    }))
    .unwrap();
    let deliver = job.deliver.unwrap();
    assert_eq!(
        (deliver.target, deliver.platform.as_deref()),
        (DeliverTarget::Telegram, Some("42"))
    );
    assert_eq!((job.repeat, job.run_count), (Some(5), 2));
    assert_eq!(job.model.unwrap().provider.as_deref(), Some("openrouter"));
    assert_eq!(job.script.as_deref(), Some("./check.sh"));
    assert_eq!(job.context_from, Some(vec!["j0".to_string()]));

    for (job, reason) in [
        (with("timezone", json!("Australia/Hobart")), "sets timezone"),
        (
            with("deliver", json!("telegram:1,discord:2")),
            "several targets",
        ),
        (
            with("deliver", json!("carrier-pigeon")),
            "platform Reformed does not support",
        ),
        (with("deliver", json!("origin")), "Hermes did not record"),
        (
            with("repeat", json!({"times": 3, "completed": 3})),
            "it has finished",
        ),
        (with("state", json!("completed")), "it has finished"),
        (
            with("script", json!("../check.sh")),
            "not a file in the scripts folder",
        ),
        (
            with("script", json!("missing.sh")),
            "not a file in the scripts folder",
        ),
        (with("id", json!("CON")), "not a safe file name"),
        (
            with("schedule", json!({"kind": "cron", "expr": "0 9 1 * 1"})),
            "both day of month",
        ),
        (
            with("schedule", json!({"kind": "cron", "expr": "0 9 * * * 30"})),
            "seconds past",
        ),
        (
            with("schedule", json!({"kind": "interval", "minutes": 45})),
            "does not divide",
        ),
        (
            with("schedule", json!({"kind": "interval", "minutes": 30})),
            "keep its timing",
        ),
        (
            with(
                "schedule",
                json!({"kind": "once", "run_at": "2026-11-01T00:00:00+00:00"}),
            ),
            "has passed",
        ),
    ] {
        let error = convert(job.clone()).unwrap_err();
        assert!(error.contains(reason), "{job}: {error}");
    }
}

#[test]
fn jobs_json_layouts_and_case_colliding_ids() {
    let list = cron::jobs(br#"{"jobs": {"k1": {"prompt": "p"}, "k2": {"id": "inline"}}}"#).unwrap();
    let ids: Vec<_> = list.iter().map(|job| job["id"].clone()).collect();
    assert_eq!(ids, [json!("k1"), json!("inline")]);
    assert_eq!(cron::jobs(b"[]").unwrap().len(), 0);
    assert!(cron::jobs(format!("{{\"x\": \"{SECRET}").as_bytes())
        .unwrap_err()
        .contains("not valid JSON"));

    let root = tempfile::tempdir().unwrap();
    let from = hermes_home(root.path());
    fs::write(
        from.join("cron/jobs.json"),
        json!([
            {"id": "Abc", "prompt": "p", "schedule": {"kind": "cron", "expr": "0 9 * * *"}},
            {"id": "abc", "prompt": "p", "schedule": {"kind": "cron", "expr": "0 9 * * *"}},
        ])
        .to_string(),
    )
    .unwrap();
    let report = run(&from, &root.path().join("reformed"), true, false);
    let text = report.lines.join("\n");
    assert!(
        text.contains("job abc: skipped, another job has the same id"),
        "{text}"
    );
}
