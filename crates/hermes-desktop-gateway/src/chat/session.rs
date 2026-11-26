//! A live session's shared state: the connections it streams to, the log of its recent events
//! for `session.events.since`, and the prompts waiting for its running turn.
//!
//! One lock guards it all, so an event's seq, its place in the log and its order on every
//! connection are decided together. Sends are to unbounded channels and never wait; a
//! connection that stops reading is closed by its write deadline (`lib.rs`), which bounds what
//! waits for it.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::Value;
use tokio::sync::OwnedSemaphorePermit;
use tokio::time::Instant;

use super::canonical::Admission;
use super::{Outbox, Outgoing};
use crate::attach::StagedImage;

/// Most events, and most bytes of them, one session's log keeps (upstream keeps 512 events
/// and 4 MiB per session in tui_gateway/event_replay.py).
const LOG_EVENTS: usize = 512;
const LOG_BYTES: usize = 1 << 20;
/// Most bytes of event logs kept across the gateway (upstream: 64 MiB). Past it an event is
/// sent but not kept, and a replay from before it reports `truncated`.
const PROCESS_LOG_BYTES: usize = 64 << 20;

/// Most prompts, and most bytes of their text, one session holds for its running turn.
pub(crate) const QUEUE_PROMPTS: usize = 16;
const QUEUE_BYTES: usize = 1 << 20;
/// Most bytes of queued prompt text across the gateway.
const PROCESS_QUEUE_BYTES: usize = 16 << 20;

/// Bytes kept in event logs and prompt queues across the gateway's sessions.
#[derive(Default)]
pub(crate) struct Budget {
    log: AtomicUsize,
    queue: AtomicUsize,
}

/// Takes `bytes` from `counter` if the total stays within `cap`.
fn reserve(counter: &AtomicUsize, bytes: usize, cap: usize) -> bool {
    counter
        .try_update(Ordering::AcqRel, Ordering::Acquire, |held| {
            held.checked_add(bytes).filter(|total| *total <= cap)
        })
        .is_ok()
}

fn release(counter: &AtomicUsize, bytes: usize) {
    counter.fetch_sub(bytes, Ordering::AcqRel);
}

/// A prompt waiting for the session's running turn.
pub(super) struct Queued {
    pub(super) text: String,
    pub(super) images: Vec<StagedImage>,
    pub(super) admission: Option<Admission>,
}

/// How a prompt was taken.
pub(super) enum Admitted {
    /// It runs now as turn `generation`, holding `slot`; the prompt comes back to be run.
    Start {
        generation: u64,
        sequence: u64,
        slot: OwnedSemaphorePermit,
        prompt: Queued,
    },
    /// It waits behind the running turn.
    Queued { sequence: u64 },
}

/// Why a prompt was not taken.
pub(super) enum Refused {
    /// The session is idle and every turn slot is taken.
    NoSlot,
    /// The session's queue, or the gateway's, is full.
    QueueFull,
}

/// What `session.events.since` answers from the log.
pub(crate) struct Replay {
    /// Each event's `params`, oldest first.
    pub(crate) events: Vec<Value>,
    pub(crate) latest_seq: u64,
    pub(crate) truncated: bool,
}

#[derive(Default)]
struct Hub {
    /// Last event number sent; the desktop orders and deduplicates by it.
    seq: u64,
    /// Connection ids and their outboxes.
    subscribers: Vec<(u64, Outbox)>,
    /// Recent events' `params` as sent, with their seq.
    log: VecDeque<(u64, Arc<str>)>,
    log_bytes: usize,
    /// Highest seq dropped from the log or never kept in it.
    evicted_through: u64,
    busy: bool,
    queue: VecDeque<Queued>,
    queue_bytes: usize,
    /// Prompts admitted (the canonical receipt's `sequence`).
    admitted: u64,
    /// Turns started (the canonical wire's execution generation).
    generation: u64,
}

pub(crate) struct LiveSession {
    hub: Mutex<Hub>,
    budget: Arc<Budget>,
    /// Images staged by `image.attach_bytes` for the next prompt.
    images: Mutex<Vec<StagedImage>>,
}

impl LiveSession {
    pub(super) fn new(budget: Arc<Budget>) -> Self {
        Self {
            hub: Mutex::default(),
            budget,
            images: Mutex::default(),
        }
    }

    fn hub(&self) -> MutexGuard<'_, Hub> {
        // Every update leaves the hub whole before it can panic (sends do not panic), so a
        // poisoned lock is still safe to use.
        self.hub.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(super) fn images(&self) -> MutexGuard<'_, Vec<StagedImage>> {
        // Pushes and takes only, so a poisoned lock is still whole.
        self.images.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Streams this session's events to connection `id` too; once per connection.
    pub(crate) fn subscribe(&self, id: u64, outbox: &Outbox) {
        let mut hub = self.hub();
        if !hub.subscribers.iter().any(|(conn, _)| *conn == id) {
            hub.subscribers.push((id, outbox.clone()));
        }
    }

    pub(super) fn unsubscribe(&self, id: u64) {
        self.hub().subscribers.retain(|(conn, _)| *conn != id);
    }

    /// Sends one event, its `params` stamped with the next seq, to every connection and keeps
    /// it in the log. `slot` is a turn's slot, freed once every connection's copy is written or
    /// dropped.
    pub(crate) fn publish(&self, mut params: Value, slot: Option<Arc<OwnedSemaphorePermit>>) {
        let mut hub = self.hub();
        hub.seq += 1;
        let seq = hub.seq;
        params["seq"] = seq.into();
        let text: Arc<str> = params.to_string().into();
        hub.keep(seq, &text, &self.budget);
        let frame: Arc<str> =
            format!(r#"{{"jsonrpc":"2.0","method":"event","params":{text}}}"#).into();
        let queued = Instant::now();
        // A connection that has closed drops out here.
        hub.subscribers.retain(|(_, outbox)| {
            outbox
                .send(Outgoing::shared(Arc::clone(&frame), queued, slot.clone()))
                .is_ok()
        });
    }

    /// The logged events after `last_seen`.
    pub(crate) fn events_since(&self, last_seen: u64) -> Replay {
        let (kept, latest_seq, truncated) = {
            let hub = self.hub();
            let kept: Vec<Arc<str>> = hub
                .log
                .iter()
                .filter(|(seq, _)| *seq > last_seen)
                .map(|(_, text)| Arc::clone(text))
                .collect();
            (kept, hub.seq, last_seen < hub.evicted_through)
        };
        // Parsed after the lock is released: a large replay does not hold up a turn's events.
        let events = kept
            .iter()
            .filter_map(|text| serde_json::from_str(text).ok())
            .collect();
        Replay {
            events,
            latest_seq,
            truncated,
        }
    }

    /// Whether a turn runs or waits to start, the last seq and the turns started.
    pub(super) fn state(&self) -> (bool, u64, u64) {
        let hub = self.hub();
        (hub.busy, hub.seq, hub.generation)
    }

    /// Takes `prompt`: it starts now when the session is idle and `slot` gives a turn slot,
    /// or waits behind the running turn. Its admission, if any, gets its sequence here and
    /// its generation when it starts.
    pub(super) fn admit(
        &self,
        slot: impl FnOnce() -> Option<OwnedSemaphorePermit>,
        mut prompt: Queued,
    ) -> Result<Admitted, (Refused, Queued)> {
        let mut hub = self.hub();
        let sequence = hub.admitted + 1;
        if let Some(admission) = prompt.admission.as_mut() {
            admission.sequence = sequence;
        }
        if !hub.busy {
            let Some(slot) = slot() else {
                return Err((Refused::NoSlot, prompt));
            };
            hub.busy = true;
            hub.admitted = sequence;
            hub.generation += 1;
            if let Some(admission) = prompt.admission.as_mut() {
                admission.generation = hub.generation;
            }
            return Ok(Admitted::Start {
                generation: hub.generation,
                sequence,
                slot,
                prompt,
            });
        }
        let bytes = prompt.text.len();
        if hub.queue.len() >= QUEUE_PROMPTS
            || hub.queue_bytes + bytes > QUEUE_BYTES
            || !reserve(&self.budget.queue, bytes, PROCESS_QUEUE_BYTES)
        {
            return Err((Refused::QueueFull, prompt));
        }
        hub.queue_bytes += bytes;
        hub.queue.push_back(prompt);
        hub.admitted = sequence;
        Ok(Admitted::Queued { sequence })
    }

    /// Called when a turn ends: the next waiting prompt and its turn's generation, or `None`
    /// once the queue is empty, which leaves the session idle.
    pub(super) fn next(&self) -> Option<(Queued, u64)> {
        let mut hub = self.hub();
        let Some(prompt) = hub.queue.pop_front() else {
            hub.busy = false;
            return None;
        };
        hub.queue_bytes -= prompt.text.len();
        release(&self.budget.queue, prompt.text.len());
        hub.generation += 1;
        let mut prompt = prompt;
        if let Some(admission) = prompt.admission.as_mut() {
            admission.generation = hub.generation;
        }
        Some((prompt, hub.generation))
    }

    /// Leaves the session idle and drops its waiting prompts, for a turn task that ends
    /// without draining them (a panic, or the runtime shutting down).
    pub(super) fn abandon(&self) {
        let mut hub = self.hub();
        hub.busy = false;
        hub.queue.clear();
        release(&self.budget.queue, hub.queue_bytes);
        hub.queue_bytes = 0;
    }
}

impl Hub {
    /// Logs event `seq`, then drops the oldest events past the session's limits.
    fn keep(&mut self, seq: u64, text: &Arc<str>, budget: &Budget) {
        let size = text.len();
        if size > LOG_BYTES || !reserve(&budget.log, size, PROCESS_LOG_BYTES) {
            self.evicted_through = seq;
            return;
        }
        self.log.push_back((seq, Arc::clone(text)));
        self.log_bytes += size;
        while self.log.len() > LOG_EVENTS || self.log_bytes > LOG_BYTES {
            let Some((old, text)) = self.log.pop_front() else {
                break;
            };
            self.log_bytes -= text.len();
            release(&budget.log, text.len());
            self.evicted_through = self.evicted_through.max(old);
        }
    }
}

impl Drop for LiveSession {
    fn drop(&mut self) {
        let hub = self.hub.get_mut().unwrap_or_else(|e| e.into_inner());
        release(&self.budget.log, hub.log_bytes);
        release(&self.budget.queue, hub.queue_bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::sync::mpsc;

    fn session() -> LiveSession {
        LiveSession::new(Arc::default())
    }

    #[test]
    fn every_subscriber_gets_each_event_in_order_and_a_closed_one_drops_out() {
        let live = session();
        let (a, mut a_rx) = mpsc::unbounded_channel();
        let (b, b_rx) = mpsc::unbounded_channel();
        live.subscribe(1, &a);
        live.subscribe(2, &b);
        live.subscribe(1, &a);
        live.publish(json!({"type": "x"}), None);
        drop(b_rx);
        live.publish(json!({"type": "y"}), None);
        let seqs: Vec<u64> = std::iter::from_fn(|| a_rx.try_recv().ok())
            .map(|out| {
                serde_json::from_str::<Value>(&out.text).unwrap()["params"]["seq"]
                    .as_u64()
                    .unwrap()
            })
            .collect();
        assert_eq!(seqs, [1, 2]);
        assert_eq!(live.hub().subscribers.len(), 1);
    }

    #[test]
    fn the_log_keeps_the_last_events_and_reports_what_it_dropped() {
        let live = session();
        for n in 0..LOG_EVENTS + 3 {
            live.publish(json!({"n": n}), None);
        }
        let all = live.events_since(0);
        assert!(all.truncated);
        assert_eq!(all.events.len(), LOG_EVENTS);
        assert_eq!(all.events[0]["seq"], 4);
        assert_eq!(all.latest_seq, (LOG_EVENTS + 3) as u64);
        let recent = live.events_since(3);
        assert!(!recent.truncated);
        assert_eq!(recent.events.len(), LOG_EVENTS);
        let none = live.events_since(all.latest_seq);
        assert!(none.events.is_empty() && !none.truncated);
    }

    #[test]
    fn an_event_past_the_gateways_log_budget_is_sent_but_not_kept() {
        let budget = Arc::new(Budget::default());
        budget.log.store(PROCESS_LOG_BYTES, Ordering::Release);
        let live = LiveSession::new(Arc::clone(&budget));
        let (tx, mut rx) = mpsc::unbounded_channel();
        live.subscribe(1, &tx);
        live.publish(json!({"type": "x"}), None);
        assert!(rx.try_recv().is_ok());
        let replay = live.events_since(0);
        assert!(replay.events.is_empty() && replay.truncated);
        assert_eq!(budget.log.load(Ordering::Acquire), PROCESS_LOG_BYTES);
    }

    #[test]
    fn dropping_a_session_returns_its_bytes() {
        let budget = Arc::new(Budget::default());
        let live = LiveSession::new(Arc::clone(&budget));
        live.publish(json!({"type": "x"}), None);
        assert!(budget.log.load(Ordering::Acquire) > 0);
        drop(live);
        assert_eq!(budget.log.load(Ordering::Acquire), 0);
    }

    fn prompt(text: &str) -> Queued {
        Queued {
            text: text.into(),
            images: Vec::new(),
            admission: None,
        }
    }

    #[test]
    fn prompts_queue_behind_the_running_turn_in_order_up_to_the_limit() {
        let live = session();
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let slot = || Arc::clone(&slots).try_acquire_owned().ok();
        assert!(matches!(
            live.admit(slot, prompt("a")),
            Ok(Admitted::Start {
                generation: 1,
                sequence: 1,
                ..
            })
        ));
        for n in 0..QUEUE_PROMPTS {
            let text = format!("q{n}");
            assert!(matches!(
                live.admit(|| None, prompt(&text)),
                Ok(Admitted::Queued { sequence }) if sequence == n as u64 + 2
            ));
        }
        assert!(matches!(
            live.admit(|| None, prompt("over")),
            Err((Refused::QueueFull, _))
        ));
        let (first, generation) = live.next().unwrap();
        assert_eq!((first.text.as_str(), generation), ("q0", 2));
        while live.next().is_some() {}
        assert!(!live.state().0);
        assert_eq!(live.budget.queue.load(Ordering::Acquire), 0);
    }

    #[test]
    fn an_idle_session_without_a_free_slot_refuses_and_stays_idle() {
        let live = session();
        assert!(matches!(
            live.admit(|| None, prompt("a")),
            Err((Refused::NoSlot, _))
        ));
        assert_eq!(live.state(), (false, 0, 0));
    }

    #[test]
    fn abandoning_drops_the_queue_and_its_bytes() {
        let live = session();
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let _ = live.admit(|| Arc::clone(&slots).try_acquire_owned().ok(), prompt("a"));
        let _ = live.admit(|| None, prompt("waiting"));
        live.abandon();
        assert!(!live.state().0);
        assert!(live.next().is_none());
        assert_eq!(live.budget.queue.load(Ordering::Acquire), 0);
    }
}
