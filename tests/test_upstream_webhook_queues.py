import contextlib
import io
import os
import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from scripts.upstream_webhook_sync import (
    SqliteQueue,
    SyncEvent,
    build_parser,
    main,
)


def push_event(delivery_id):
    return SyncEvent(
        delivery_id=delivery_id,
        event_name="push",
        repository="example/upstream",
        ref="refs/heads/main",
        before_sha="a" * 40,
        after_sha="b" * 40,
        compare_url="https://example.test/compare",
        pushed_at="2026-11-20T00:00:00Z",
        payload={"commits": [{"message": "test delivery"}]},
    )


class WebhookQueueTests(unittest.TestCase):
    def test_sqlite_persists_delivery_and_stops_after_retry_limit(self):
        first = push_event("first")
        second = push_event("second")
        with tempfile.TemporaryDirectory() as scratch:
            db_path = str(Path(scratch) / "queue.sqlite")
            listener = SqliteQueue(db_path)
            self.assertTrue(listener.enqueue(first))
            self.assertFalse(listener.enqueue(first))
            self.assertTrue(listener.enqueue(second))

            worker = SqliteQueue(db_path)
            item = worker.claim_next()
            self.assertEqual((item.event, item.attempts), (first, 1))
            worker.mark_retry_or_dead(item.id, item.attempts, 2, "first failure")
            retry = SqliteQueue(db_path).claim_next()
            self.assertEqual((retry.id, retry.event, retry.attempts), (item.id, first, 2))
            worker.mark_retry_or_dead(retry.id, retry.attempts, 2, "final failure")

            next_item = worker.claim_next()
            self.assertEqual((next_item.event, next_item.attempts), (second, 1))
            self.assertIsNone(SqliteQueue(db_path).claim_next())
            worker.mark_done(next_item.id)

            reopened = SqliteQueue(db_path)
            self.assertIsNone(reopened.claim_next())
            self.assertFalse(reopened.enqueue(second))

    def test_sqlite_releases_connections_after_commit_and_duplicate_rollback(self):
        connections = []
        connect = sqlite3.connect

        def track_connection(*args, **kwargs):
            connection = connect(*args, **kwargs)
            connections.append(connection)
            return connection

        with tempfile.TemporaryDirectory() as scratch:
            try:
                with patch.object(sqlite3, "connect", side_effect=track_connection):
                    queue = SqliteQueue(str(Path(scratch) / "queue.sqlite"))
                    event = push_event("closed")
                    self.assertTrue(queue.enqueue(event))
                    self.assertFalse(queue.enqueue(event))
                self.assertEqual(len(connections), 3)
                for connection in connections:
                    with self.assertRaisesRegex(sqlite3.ProgrammingError, "closed database"):
                        connection.execute("SELECT 1")
            finally:
                for connection in connections:
                    connection.close()

    def test_retired_backends_are_rejected_by_both_subcommands(self):
        parser = build_parser()
        for command in ("listen", "worker"):
            for backend in ("sqs", "kafka"):
                with self.subTest(command=command, backend=backend):
                    with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
                        parser.parse_args([command, "--backend", backend])
                    self.assertEqual(error.exception.code, 2)

    def test_retired_backend_environment_is_rejected_before_startup(self):
        with (
            patch("scripts.upstream_webhook_sync.serve_webhook") as serve,
            patch("scripts.upstream_webhook_sync.worker_loop") as work,
        ):
            for command in ("listen", "worker"):
                for backend in ("sqs", "kafka"):
                    for arguments in ([], ["--backend", "sqlite"]):
                        with self.subTest(command=command, backend=backend, arguments=arguments):
                            stderr = io.StringIO()
                            with (
                                patch.dict(os.environ, {"UPSTREAM_SYNC_BACKEND": backend}),
                                patch.object(sys, "argv", ["upstream_webhook_sync.py", command, *arguments]),
                                contextlib.redirect_stderr(stderr),
                                self.assertRaises(SystemExit) as error,
                            ):
                                main()
                            self.assertEqual(error.exception.code, 2)
                            self.assertIn("UPSTREAM_SYNC_BACKEND must be sqlite", stderr.getvalue())
            serve.assert_not_called()
            work.assert_not_called()
