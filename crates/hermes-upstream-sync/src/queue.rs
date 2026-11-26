//! Queue backends. SQLite is the default and the only one ported so far; the
//! listener and worker see a backend only through these traits, so SQS or
//! Kafka can be added beside it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{params, Connection, ErrorCode, OptionalExtension, TransactionBehavior};

use crate::event::SyncEvent;

/// Where the listener puts accepted deliveries.
pub trait Publisher: Send + Sync {
    /// Queues the event; `Ok(false)` when its delivery id, or a body with the
    /// same SHA-256, was already queued.
    fn enqueue(&self, event: &SyncEvent, body_sha256: &str) -> Result<bool, String>;
}

#[derive(Debug, PartialEq)]
#[allow(dead_code)] // claimed by the worker, which lands next
pub struct QueueItem {
    pub id: i64,
    pub attempts: i64,
    pub event: SyncEvent,
}

/// The script's `SqliteQueue`: same file, schema and statuses, so either
/// implementation reads what the other wrote. Each call opens its own
/// connection and closes it on return.
pub struct SqliteQueue {
    path: PathBuf,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS queue_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  delivery_id TEXT NOT NULL UNIQUE,
  event_name TEXT NOT NULL,
  repository TEXT NOT NULL,
  ref TEXT NOT NULL,
  before_sha TEXT NOT NULL,
  after_sha TEXT NOT NULL,
  compare_url TEXT NOT NULL,
  pushed_at TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending',
  attempts INTEGER NOT NULL DEFAULT 0,
  last_error TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_queue_events_status_created ON queue_events(status, id);
";

/// Added beside the script's schema: the SHA-256 of the body each delivery
/// came with, unique where known. The script writes NULL there and reads
/// rows by column name, so it still shares the file.
const BODY_HASH_COLUMN: &str = "ALTER TABLE queue_events ADD COLUMN body_sha256 TEXT";
const BODY_HASH_INDEX: &str = "CREATE UNIQUE INDEX IF NOT EXISTS idx_queue_events_body_sha256      ON queue_events(body_sha256) WHERE body_sha256 IS NOT NULL";

#[allow(dead_code)] // the worker's side lands next
impl SqliteQueue {
    pub fn open(path: &Path) -> Result<SqliteQueue, String> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
        }
        let queue = SqliteQueue {
            path: path.to_path_buf(),
        };
        let mut conn = queue.connect()?;
        // One writer at a time, so a listener and worker opening the same
        // file cannot both add the column.
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        tx.execute_batch(SCHEMA).map_err(db_error)?;
        let has_hash = tx
            .prepare("SELECT 1 FROM pragma_table_info('queue_events') WHERE name = 'body_sha256'")
            .and_then(|mut s| s.exists([]))
            .map_err(db_error)?;
        if !has_hash {
            tx.execute_batch(BODY_HASH_COLUMN).map_err(db_error)?;
        }
        tx.execute_batch(BODY_HASH_INDEX).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(queue)
    }

    fn connect(&self) -> Result<Connection, String> {
        let conn = Connection::open(&self.path).map_err(db_error)?;
        conn.busy_timeout(Duration::from_secs(30))
            .map_err(db_error)?;
        Ok(conn)
    }

    /// Oldest pending event, marked `processing` with one more attempt.
    pub fn claim_next(&self) -> Result<Option<QueueItem>, String> {
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let row = tx
            .query_row(
                "SELECT id, attempts, delivery_id, event_name, repository, ref, before_sha,
                        after_sha, compare_url, pushed_at, payload_json
                 FROM queue_events WHERE status = 'pending' ORDER BY id ASC LIMIT 1",
                [],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        [2, 3, 4, 5, 6, 7, 8, 9, 10].map(|i| r.get::<_, String>(i)),
                    ))
                },
            )
            .optional()
            .map_err(db_error)?;
        let Some((id, attempts, columns)) = row else {
            tx.commit().map_err(db_error)?;
            return Ok(None);
        };
        let [delivery_id, event_name, repository, git_ref, before_sha, after_sha, compare_url, pushed_at, payload_json] =
            columns.map(|c| c.map_err(db_error));
        let attempts = attempts + 1;
        tx.execute(
            "UPDATE queue_events SET status='processing', attempts=?1, updated_at=?2 WHERE id=?3",
            params![attempts, hermes_py::utc_isoformat(), id],
        )
        .map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        let payload = hermes_py::loads(&payload_json?)
            .map_err(|e| format!("queued payload {id} is not JSON: {e}"))?;
        Ok(Some(QueueItem {
            id,
            attempts,
            event: SyncEvent {
                delivery_id: delivery_id?,
                event_name: event_name?,
                repository: repository?,
                git_ref: git_ref?,
                before_sha: before_sha?,
                after_sha: after_sha?,
                compare_url: compare_url?,
                pushed_at: pushed_at?,
                payload,
            },
        }))
    }

    pub fn mark_done(&self, id: i64, status: &str, note: Option<&str>) -> Result<(), String> {
        self.set_status(id, status, note)
    }

    /// Back to `pending`, or `dead` once `attempts` reaches `max_attempts`.
    pub fn mark_retry_or_dead(
        &self,
        id: i64,
        attempts: i64,
        max_attempts: i64,
        error: &str,
    ) -> Result<(), String> {
        let status = if attempts >= max_attempts {
            "dead"
        } else {
            "pending"
        };
        self.set_status(id, status, Some(error))
    }

    fn set_status(&self, id: i64, status: &str, note: Option<&str>) -> Result<(), String> {
        self.connect()?
            .execute(
                "UPDATE queue_events SET status=?1, last_error=?2, updated_at=?3 WHERE id=?4",
                params![status, note, hermes_py::utc_isoformat(), id],
            )
            .map(drop)
            .map_err(db_error)
    }
}

impl Publisher for SqliteQueue {
    fn enqueue(&self, event: &SyncEvent, body_sha256: &str) -> Result<bool, String> {
        let now = hermes_py::utc_isoformat();
        let result = self.connect()?.execute(
            "INSERT INTO queue_events(
               delivery_id, event_name, repository, ref, before_sha, after_sha,
               compare_url, pushed_at, payload_json, status, attempts, created_at, updated_at,
               body_sha256
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending', 0, ?10, ?10, ?11)",
            params![
                event.delivery_id,
                event.event_name,
                event.repository,
                event.git_ref,
                event.before_sha,
                event.after_sha,
                event.compare_url,
                event.pushed_at,
                hermes_py::dumps(&event.payload, None, false),
                now,
                body_sha256,
            ],
        );
        match result {
            Ok(_) => Ok(true),
            // A repeated GitHub delivery id or body is accepted without a
            // second row.
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == ErrorCode::ConstraintViolation =>
            {
                Ok(false)
            }
            Err(e) => Err(db_error(e)),
        }
    }
}

fn db_error(e: rusqlite::Error) -> String {
    format!("queue database error: {e}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_event(delivery_id: &str) -> SyncEvent {
        SyncEvent {
            delivery_id: delivery_id.to_string(),
            event_name: "push".to_string(),
            repository: "example/upstream".to_string(),
            git_ref: "refs/heads/main".to_string(),
            before_sha: "a".repeat(40),
            after_sha: "b".repeat(40),
            compare_url: "https://example.test/compare".to_string(),
            pushed_at: "2026-11-20T00:00:00Z".to_string(),
            payload: hermes_py::loads(r#"{"commits": [{"message": "test delivery"}]}"#).unwrap(),
        }
    }

    // tests/test_upstream_webhook_queues.py::test_sqlite_persists_delivery_and_stops_after_retry_limit
    #[test]
    fn sqlite_persists_delivery_and_stops_after_retry_limit() {
        let scratch = tempfile::tempdir().unwrap();
        let db = scratch.path().join("queue.sqlite");
        let (first, second) = (push_event("first"), push_event("second"));
        let listener = SqliteQueue::open(&db).unwrap();
        assert!(listener.enqueue(&first, "h1").unwrap());
        assert!(!listener.enqueue(&first, "h1").unwrap());
        assert!(listener.enqueue(&second, "h2").unwrap());

        let worker = SqliteQueue::open(&db).unwrap();
        let item = worker.claim_next().unwrap().unwrap();
        assert_eq!((&item.event, item.attempts), (&first, 1));
        worker
            .mark_retry_or_dead(item.id, item.attempts, 2, "first failure")
            .unwrap();
        let retry = SqliteQueue::open(&db)
            .unwrap()
            .claim_next()
            .unwrap()
            .unwrap();
        assert_eq!(
            (retry.id, &retry.event, retry.attempts),
            (item.id, &first, 2)
        );
        worker
            .mark_retry_or_dead(retry.id, retry.attempts, 2, "final failure")
            .unwrap();

        let next = worker.claim_next().unwrap().unwrap();
        assert_eq!((&next.event, next.attempts), (&second, 1));
        assert!(SqliteQueue::open(&db)
            .unwrap()
            .claim_next()
            .unwrap()
            .is_none());
        worker.mark_done(next.id, "done", None).unwrap();

        let reopened = SqliteQueue::open(&db).unwrap();
        assert!(reopened.claim_next().unwrap().is_none());
        assert!(!reopened.enqueue(&second, "h2").unwrap());
        let statuses: Vec<(String, Option<String>)> = Connection::open(&db)
            .unwrap()
            .prepare("SELECT status, last_error FROM queue_events ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            statuses,
            [
                ("dead".to_string(), Some("final failure".to_string())),
                ("done".to_string(), None)
            ]
        );
    }

    // tests/test_upstream_webhook_queues.py::test_sqlite_releases_connections_after_commit_and_duplicate_rollback:
    // no connection outlives a call, so the file can be removed straight
    // after. Only Windows refuses to delete a file a connection still holds,
    // so only there can this fail; elsewhere it holds by construction
    // (`SqliteQueue` keeps a path, not a connection).
    #[test]
    fn sqlite_releases_connections_after_commit_and_duplicate() {
        let scratch = tempfile::tempdir().unwrap();
        let db = scratch.path().join("queue.sqlite");
        let queue = SqliteQueue::open(&db).unwrap();
        let event = push_event("closed");
        assert!(queue.enqueue(&event, "h").unwrap());
        assert!(!queue.enqueue(&event, "h").unwrap());
        std::fs::remove_file(&db).unwrap();
    }

    #[test]
    fn payload_is_stored_as_python_json_dumps_wrote_it() {
        let scratch = tempfile::tempdir().unwrap();
        let db = scratch.path().join("q.db");
        let queue = SqliteQueue::open(&db).unwrap();
        let mut event = push_event("unicode");
        event.payload = hermes_py::loads(r#"{"b":1,"a":"é"}"#).unwrap();
        queue.enqueue(&event, "h").unwrap();
        let stored: String = Connection::open(&db)
            .unwrap()
            .query_row("SELECT payload_json FROM queue_events", [], |r| r.get(0))
            .unwrap();
        let backslash = char::from(92u8);
        assert_eq!(stored, format!(r#"{{"b": 1, "a": "{backslash}u00e9"}}"#));
    }

    #[test]
    fn a_file_the_script_made_gains_the_body_column_and_keeps_its_rows() {
        let scratch = tempfile::tempdir().unwrap();
        let db = scratch.path().join("script.db");
        let script = Connection::open(&db).unwrap();
        script.execute_batch(SCHEMA).unwrap();
        // The script's INSERT, which names no body column.
        let insert = "INSERT INTO queue_events(delivery_id, event_name, repository, ref,
              before_sha, after_sha, compare_url, pushed_at, payload_json, status, attempts,
              created_at, updated_at)
            VALUES (?1, 'push', 'r', 'refs/heads/main', 'a', 'b', 'c', 'p', '{}', 'pending', 0, 't', 't')";
        script.execute(insert, ["by-script"]).unwrap();
        drop(script);
        let queue = SqliteQueue::open(&db).unwrap();
        SqliteQueue::open(&db).unwrap(); // a second open adds nothing twice
        assert!(queue.enqueue(&push_event("one"), "same-body").unwrap());
        assert!(!queue.enqueue(&push_event("two"), "same-body").unwrap());
        assert!(queue.enqueue(&push_event("three"), "other-body").unwrap());
        // The script keeps writing rows with no body hash after the change.
        let script = Connection::open(&db).unwrap();
        script.execute(insert, ["by-script-2"]).unwrap();
        let ids: Vec<String> = script
            .prepare("SELECT delivery_id FROM queue_events ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(ids, ["by-script", "one", "three", "by-script-2"]);
        assert_eq!(
            queue.claim_next().unwrap().unwrap().event.delivery_id,
            "by-script"
        );
    }
}
