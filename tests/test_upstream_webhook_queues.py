import json
import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

from scripts.upstream_webhook_sync import (
    KafkaConsumerWrapper,
    KafkaPublisher,
    SqliteQueue,
    SqsConsumer,
    SqsPublisher,
    SyncEvent,
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

    def test_sqs_round_trip_keeps_the_delivery_key_and_explicit_ack(self):
        event = push_event("sqs-delivery")
        queue_url = "https://example.test/queue.fifo"
        client = Mock()
        boto3 = SimpleNamespace(client=Mock(return_value=client))
        with patch.dict(sys.modules, {"boto3": boto3}):
            publisher = SqsPublisher(queue_url, "ap-southeast-2")
            consumer = SqsConsumer(queue_url, "ap-southeast-2")

        self.assertTrue(publisher.enqueue(event))
        sent = client.send_message.call_args.kwargs
        self.assertEqual(sent["QueueUrl"], queue_url)
        self.assertEqual(sent["MessageGroupId"], "upstream-sync")
        self.assertEqual(sent["MessageDeduplicationId"], event.delivery_id)
        client.receive_message.return_value = {
            "Messages": [{"Body": sent["MessageBody"], "ReceiptHandle": "test-receipt"}]
        }
        self.assertEqual(consumer.receive(30), (event, "test-receipt"))
        self.assertEqual(client.receive_message.call_args.kwargs["WaitTimeSeconds"], 20)
        client.delete_message.assert_not_called()
        consumer.ack("test-receipt")
        client.delete_message.assert_called_once_with(QueueUrl=queue_url, ReceiptHandle="test-receipt")

    def test_kafka_round_trip_waits_for_publish_and_retains_explicit_commit(self):
        event = push_event("kafka-delivery")
        producer = Mock()
        consumer = Mock()
        kafka = SimpleNamespace(
            KafkaProducer=Mock(return_value=producer),
            KafkaConsumer=Mock(return_value=consumer),
        )
        with patch.dict(sys.modules, {"kafka": kafka}):
            publisher = KafkaPublisher("broker.test:9092", "upstream")
            reader = KafkaConsumerWrapper("broker.test:9092", "upstream", "sync-worker")

        self.assertTrue(publisher.enqueue(event))
        producer.send.return_value.get.assert_called_once_with(timeout=10)
        args, sent = producer.send.call_args
        self.assertEqual(args, ("upstream",))
        serializers = kafka.KafkaProducer.call_args.kwargs
        self.assertEqual(serializers["key_serializer"](sent["key"]), b"kafka-delivery")
        encoded = serializers["value_serializer"](sent["value"])
        configuration = kafka.KafkaConsumer.call_args.kwargs
        self.assertFalse(configuration["enable_auto_commit"])
        record = SimpleNamespace(value=configuration["value_deserializer"](encoded))
        self.assertEqual(record.value, json.loads(event.to_json()))
        consumer.poll.return_value = {"fixture-partition": [record]}
        self.assertEqual(reader.poll(), (event, record))
        consumer.commit.assert_not_called()
        reader.ack()
        consumer.commit.assert_called_once_with()
