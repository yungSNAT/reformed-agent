//! A live session's shared state: the connections it streams to, the log of its recent events
//! for `session.events.since`, and the prompts waiting for its running turn.
//!
//! One lock guards it all, so an event's seq, its place in the log and its order on every
//! connection are decided together. Sends are to unbounded channels and never wait; a
//! connection that stops reading is closed by its write deadline (`lib.rs`), which bounds what
//! waits for it.
//!
//! The gateway's log budget is shared: a session short of it clears the logs of idle sessions,
//! the least recently written first. It only tries their locks, never waits for them, as it
//! holds its own.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

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
/// Most bytes of event logs kept across the gateway (upstream: 64 MiB). Past it, idle
/// sessions' logs are cleared to make room; with none to clear, an event is sent but not kept,
/// and a replay from before it reports `truncated`.
const PROCESS_LOG_BYTES: usize = 64 << 20;

/// Most prompts, and most bytes of their text, one session holds for its running turn.
pub(crate) const QUEUE_PROMPTS: usize = 16;
const QUEUE_BYTES: usize = 1 << 20;
/// Most bytes of queued prompt text across the gateway.
const PROCESS_QUEUE_BYTES: usize = 16 << 20;

/// Most images one session holds staged for its next prompt, and their total bytes.
pub(crate) const MAX_STAGED_IMAGES: usize = 16;
pub(crate) const MAX_STAGED_BYTES: usize = 64 << 20;

/// Bytes kept in event logs and prompt queues across the gateway's sessions.
#[derive(Default)]
pub(crate) struct Budget {
    log: AtomicUsize,
    queue: AtomicUsize,
    /// Every session made with this budget, to take log bytes from.
    sessions: Mutex<Vec<Weak<LiveSession>>>,
    /// Orders sessions by when their logs were last written.
    clock: AtomicU64,
}

impl Budget {
    /// Clears idle sessions' logs, the least recently written first, until `size` bytes are
    /// reserved; false when no idle session other than `me` has a log to clear.
    fn reclaim(&self, me: &LiveSession, size: usize) -> bool {
        let mut idle: Vec<(u64, Arc<LiveSession>)> = {
            let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            sessions.retain(|session| session.strong_count() > 0);
            sessions
                .iter()
                .filter_map(Weak::upgrade)
                .filter(|session| !std::ptr::eq(Arc::as_ptr(session), me))
                .filter_map(|session| {
                    // Only tried: another session's publisher may be waiting for `me`'s lock.
                    let touched = {
                        let hub = session.hub.try_lock().ok()?;
                        (!hub.busy && hub.log_bytes > 0).then_some(hub.touched)?
                    };
                    Some((touched, session))
                })
                .collect()
        };
        idle.sort_by_key(|(touched, _)| *touched);
        for (_, session) in idle {
            if let Ok(mut hub) = session.hub.try_lock() {
                if !hub.busy {
                    hub.clear_log(self);
                }
            }
            if reserve(&self.log, size, PROCESS_LOG_BYTES) {
                return true;
            }
        }
        false
    }
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
    /// The connection that sent it.
    pub(super) owner: u64,
    pub(super) text: String,
    pub(super) images: Vec<StagedImage>,
    pub(super) admission: Option<Box<Admission>>,
}

impl Queued {
    /// Bytes it holds that the queue limits count: its text, and its admission's copy.
    fn size(&self) -> usize {
        self.text.len() + self.admission.as_ref().map_or(0, |a| a.held_bytes())
    }
}

/// What a session's runner does when a turn ends.
pub(super) enum Next {
    /// Run this prompt with this slot.
    Run(Queued, OwnedSemaphorePermit),
    /// A prompt waits: get a turn slot and ask again.
    Wait,
    /// Nothing waits; the session is idle.
    Idle,
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
    /// The budget's clock when the log was last written.
    touched: u64,
    /// A turn runs, or the next waiting prompt waits for a turn slot (`waits_for_slot`).
    busy: bool,
    waits_for_slot: bool,
    queue: VecDeque<Queued>,
    queue_bytes: usize,
    /// Prompts admitted (the canonical receipt's `sequence`).
    admitted: u64,
    /// Turns started (the canonical wire's execution generation).
    generation: u64,
    /// The running turn's admission, on the canonical wire.
    current: Option<Box<Admission>>,
}

pub(crate) struct LiveSession {
    hub: Mutex<Hub>,
    budget: Arc<Budget>,
    /// Images staged by `image.attach_bytes` for the next prompt.
    images: Mutex<Vec<StagedImage>>,
}

impl LiveSession {
    pub(super) fn new(budget: Arc<Budget>) -> Arc<Self> {
        let session = Arc::new(Self {
            hub: Mutex::default(),
            budget: Arc::clone(&budget),
            images: Mutex::default(),
        });
        budget
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Arc::downgrade(&session));
        session
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

    /// Whether an image of `bytes` more would stay within [`MAX_STAGED_IMAGES`] and
    /// [`MAX_STAGED_BYTES`]; a method may ask before it reads or writes an image.
    pub(crate) fn has_room_for_image(&self, bytes: usize) -> bool {
        fits(&self.images(), bytes)
    }

    /// Stages `image` for the next prompt if the session stays within [`MAX_STAGED_IMAGES`]
    /// and [`MAX_STAGED_BYTES`], answering how many it then holds; past them it hands the
    /// image back, whose file the caller removes. Every method that stages an image calls it.
    pub(crate) fn stage_image(&self, image: StagedImage) -> Result<usize, StagedImage> {
        let mut images = self.images();
        if !fits(&images, image.bytes) {
            return Err(image);
        }
        images.push(image);
        Ok(images.len())
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
    /// it in the log. `slot` is a turn's slot and the connection that prompted the turn: it
    /// rides on that connection's copy only, and frees once that copy is written or dropped,
    /// or at once when that connection no longer streams the session.
    pub(crate) fn publish(
        &self,
        mut params: Value,
        slot: Option<(u64, Arc<OwnedSemaphorePermit>)>,
    ) {
        let (owner, mut slot) = match slot {
            Some((owner, slot)) => (Some(owner), Some(slot)),
            None => (None, None),
        };
        let mut hub = self.hub();
        hub.seq += 1;
        let seq = hub.seq;
        params["seq"] = seq.into();
        let text: Arc<str> = params.to_string().into();
        self.keep(&mut hub, seq, &text);
        let frame: Arc<str> =
            format!(r#"{{"jsonrpc":"2.0","method":"event","params":{text}}}"#).into();
        let queued = Instant::now();
        // A connection that has closed drops out here.
        hub.subscribers.retain(|(conn, outbox)| {
            let carried = if owner == Some(*conn) {
                slot.take()
            } else {
                None
            };
            outbox
                .send(Outgoing::shared(Arc::clone(&frame), queued, carried))
                .is_ok()
        });
    }

    /// Logs event `seq`, then drops the oldest events past the session's limits.
    fn keep(&self, hub: &mut Hub, seq: u64, text: &Arc<str>) {
        let size = text.len();
        let budget = &self.budget;
        if size > LOG_BYTES
            || !(reserve(&budget.log, size, PROCESS_LOG_BYTES) || budget.reclaim(self, size))
        {
            hub.evicted_through = seq;
            return;
        }
        hub.touched = budget.clock.fetch_add(1, Ordering::Relaxed);
        hub.log.push_back((seq, Arc::clone(text)));
        hub.log_bytes += size;
        while hub.log.len() > LOG_EVENTS || hub.log_bytes > LOG_BYTES {
            let Some((old, text)) = hub.log.pop_front() else {
                break;
            };
            hub.log_bytes -= text.len();
            release(&budget.log, text.len());
            hub.evicted_through = hub.evicted_through.max(old);
        }
    }

    /// The admissions waiting in `pending` (upstream's snapshot and `session.info` field): the
    /// running turn's when `running` is asked for, then each queued one in order.
    pub(super) fn pending(&self, session_id: &str, running: bool) -> Vec<Value> {
        self.hub().pending(session_id, running)
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

    /// Whether a turn runs (not one waiting for a slot), the last seq and the turns started.
    #[cfg(test)]
    fn state(&self) -> (bool, u64, u64) {
        self.status("").0
    }

    /// [`Self::state`] and the running and queued admissions, taken together.
    pub(super) fn status(&self, session_id: &str) -> ((bool, u64, u64), Vec<Value>) {
        let hub = self.hub();
        let running = hub.busy && !hub.waits_for_slot;
        let pending = hub.pending(session_id, running);
        ((running, hub.seq, hub.generation), pending)
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
            hub.current = prompt.admission.clone();
            return Ok(Admitted::Start {
                generation: hub.generation,
                sequence,
                slot,
                prompt,
            });
        }
        let bytes = prompt.size();
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

    /// Called when a turn ends, and again with a turn slot in `slot` after [`Next::Wait`]:
    /// the next waiting prompt runs as the next generation only once it has a slot, and an
    /// empty queue leaves the session idle.
    pub(super) fn next(&self, slot: &mut Option<OwnedSemaphorePermit>) -> Next {
        let mut hub = self.hub();
        hub.current = None;
        hub.waits_for_slot = false;
        if hub.queue.is_empty() {
            hub.busy = false;
            return Next::Idle;
        }
        let Some(permit) = slot.take() else {
            hub.waits_for_slot = true;
            return Next::Wait;
        };
        let Some(mut prompt) = hub.queue.pop_front() else {
            unreachable!("the queue was not empty");
        };
        hub.queue_bytes -= prompt.size();
        release(&self.budget.queue, prompt.size());
        hub.generation += 1;
        if let Some(admission) = prompt.admission.as_mut() {
            admission.generation = hub.generation;
        }
        hub.current = prompt.admission.clone();
        Next::Run(prompt, permit)
    }

    /// Marks the idle session busy with nothing queued, so a change to its saved rows can run
    /// with no turn's save between its check and its write; false whilst a turn runs or a
    /// prompt waits. Prompts sent during the hold queue as behind a turn.
    #[allow(dead_code, reason = "for session.undo and session.compress")]
    pub(super) fn try_hold(&self) -> bool {
        let mut hub = self.hub();
        if hub.busy || !hub.queue.is_empty() {
            return false;
        }
        hub.busy = true;
        true
    }

    /// Ends a hold: true when prompts queued during it wait for a runner (the session stays
    /// busy for it), else the session is idle again.
    #[allow(dead_code, reason = "for session.undo and session.compress")]
    pub(super) fn release_hold(&self) -> bool {
        let mut hub = self.hub();
        if hub.queue.is_empty() {
            hub.busy = false;
            return false;
        }
        true
    }

    /// Marks the running turn finished, just before its end event.
    pub(super) fn end_turn(&self) {}

    /// Leaves the session idle and drops its waiting prompts, for a turn task that ends
    /// without draining them (a panic, or the runtime shutting down).
    pub(super) fn abandon(&self) {
        let mut hub = self.hub();
        hub.busy = false;
        hub.waits_for_slot = false;
        hub.current = None;
        hub.queue.clear();
        release(&self.budget.queue, hub.queue_bytes);
        hub.queue_bytes = 0;
    }
}

/// Whether `staged` and one more image of `bytes` stay within the staging limits.
fn fits(staged: &[StagedImage], bytes: usize) -> bool {
    let held: usize = staged.iter().map(|image| image.bytes).sum();
    staged.len() < MAX_STAGED_IMAGES && held.saturating_add(bytes) <= MAX_STAGED_BYTES
}

impl Hub {
    /// Drops the whole log, so a replay from before now reports `truncated`.
    fn clear_log(&mut self, budget: &Budget) {
        release(&budget.log, self.log_bytes);
        self.log.clear();
        self.log_bytes = 0;
        self.evicted_through = self.seq;
    }

    /// See [`LiveSession::pending`].
    fn pending(&self, session_id: &str, running: bool) -> Vec<Value> {
        let current = self.current.iter().filter(|_| running && self.busy);
        let started = current.map(|admission| admission.row(session_id, true));
        let queued = self
            .queue
            .iter()
            .filter_map(|prompt| prompt.admission.as_ref())
            .map(|admission| admission.row(session_id, false));
        started.chain(queued).collect()
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

    fn session() -> Arc<LiveSession> {
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

    /// A session short of the gateway's log budget takes it from the idle session whose log
    /// was written longest ago, which then reports its replay as truncated; a session with a
    /// running turn keeps its log.
    #[test]
    fn a_session_short_of_the_log_budget_takes_it_from_the_least_recent_idle_session() {
        let budget = Arc::new(Budget::default());
        let old = LiveSession::new(Arc::clone(&budget));
        let running = LiveSession::new(Arc::clone(&budget));
        let new = LiveSession::new(Arc::clone(&budget));
        running.publish(json!({"n": 1}), None);
        old.publish(json!({"n": 2}), None);
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        assert!(matches!(
            running.admit(|| Arc::clone(&slots).try_acquire_owned().ok(), prompt("a")),
            Ok(Admitted::Start { .. })
        ));
        let held = budget.log.load(Ordering::Acquire);
        budget
            .log
            .fetch_add(PROCESS_LOG_BYTES - held, Ordering::AcqRel);

        new.publish(json!({"n": 3}), None);
        let kept = new.events_since(0);
        assert_eq!((kept.events.len(), kept.truncated), (1, false));
        let taken = old.events_since(0);
        assert!(taken.events.is_empty() && taken.truncated);
        assert_eq!(running.events_since(0).events.len(), 1);
        assert_eq!(budget.log.load(Ordering::Acquire), PROCESS_LOG_BYTES);
    }

    /// A turn's slot rides only on the copy of its last event for the connection that
    /// prompted it, so a viewer that stops reading holds no slot.
    #[test]
    fn a_turns_slot_rides_only_on_the_prompting_connections_copy() {
        let live = session();
        let (owner, mut owner_rx) = mpsc::unbounded_channel();
        let (viewer, mut viewer_rx) = mpsc::unbounded_channel();
        live.subscribe(1, &owner);
        live.subscribe(2, &viewer);
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let slot = Arc::new(Arc::clone(&slots).try_acquire_owned().unwrap());
        live.publish(json!({"type": "message.complete"}), Some((1, slot)));
        assert_eq!(slots.available_permits(), 0);
        drop(owner_rx.try_recv().unwrap());
        assert_eq!(slots.available_permits(), 1, "the viewer's copy holds it");
        assert!(viewer_rx.try_recv().is_ok());

        // With the prompting connection gone, no copy holds it.
        live.unsubscribe(1);
        let slot = Arc::new(Arc::clone(&slots).try_acquire_owned().unwrap());
        live.publish(json!({"type": "message.complete"}), Some((1, slot)));
        assert_eq!(slots.available_permits(), 1);
        assert!(viewer_rx.try_recv().is_ok());
    }

    fn image(bytes: usize) -> StagedImage {
        StagedImage {
            path: std::path::PathBuf::from("staged.png"),
            mime: "image/png",
            bytes,
        }
    }

    #[test]
    fn a_session_stages_at_most_16_images_and_64_mib() {
        let live = session();
        for n in 1..=MAX_STAGED_IMAGES {
            assert_eq!(live.stage_image(image(1)).ok(), Some(n));
        }
        assert!(!live.has_room_for_image(0));
        assert!(live.stage_image(image(1)).is_err(), "a 17th image");

        let live = session();
        assert!(live.stage_image(image(MAX_STAGED_BYTES - 1)).is_ok());
        assert!(live.has_room_for_image(1));
        assert!(live.stage_image(image(2)).is_err(), "a byte over 64 MiB");
        assert_eq!(live.stage_image(image(1)).ok(), Some(2));
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
            owner: 0,
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
        assert!(matches!(live.next(&mut None), Next::Wait));
        assert_eq!(live.state().2, 1, "no generation before a slot");
        assert!(
            !live.state().0,
            "no turn runs whilst the next waits for a slot"
        );
        let freed = Arc::new(tokio::sync::Semaphore::new(1));
        let mut slot = Arc::clone(&freed).try_acquire_owned().ok();
        let Next::Run(first, _) = live.next(&mut slot) else {
            panic!("a slot was given");
        };
        assert_eq!((first.text.as_str(), live.state().2), ("q0", 2));
        assert!(live.state().0, "the next turn runs once it has a slot");
        loop {
            let mut slot = Arc::clone(&freed).try_acquire_owned().ok();
            if matches!(live.next(&mut slot), Next::Idle) {
                break;
            }
        }
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
    fn a_hold_waits_for_an_idle_session_and_queues_prompts_until_it_ends() {
        let live = session();
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let slot = || Arc::clone(&slots).try_acquire_owned().ok();
        assert!(matches!(
            live.admit(slot, prompt("a")),
            Ok(Admitted::Start { .. })
        ));
        assert!(!live.try_hold(), "a turn runs");
        assert!(matches!(live.next(&mut None), Next::Idle));
        assert!(live.try_hold());
        assert!(!live.state().0, "no turn runs during a hold");
        assert!(!live.try_hold(), "already held");
        assert!(matches!(
            live.admit(slot, prompt("b")),
            Ok(Admitted::Queued { .. })
        ));
        assert!(live.release_hold(), "a prompt waits for a runner");
        let mut given = slot();
        assert!(matches!(live.next(&mut given), Next::Run(..)));
        assert!(live.state().0, "the queued prompt runs once the hold ends");
        assert!(matches!(live.next(&mut None), Next::Idle));
        assert!(live.try_hold());
        assert!(!live.release_hold(), "nothing waits");
        assert!(!live.state().0);
    }

    #[test]
    fn abandoning_drops_the_queue_and_its_bytes() {
        let live = session();
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let _ = live.admit(|| Arc::clone(&slots).try_acquire_owned().ok(), prompt("a"));
        let _ = live.admit(|| None, prompt("waiting"));
        live.abandon();
        assert!(!live.state().0);
        assert!(matches!(live.next(&mut None), Next::Idle));
        assert_eq!(live.budget.queue.load(Ordering::Acquire), 0);
    }

    #[test]
    fn a_runner_abandoned_whilst_its_next_prompt_waits_for_a_slot_leaves_the_session_idle() {
        let live = session();
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let _ = live.admit(|| Arc::clone(&slots).try_acquire_owned().ok(), prompt("a"));
        let _ = live.admit(|| None, prompt("waiting"));
        assert!(matches!(live.next(&mut None), Next::Wait));
        live.abandon();
        assert_eq!(live.status("s"), ((false, 0, 1), Vec::new()));
        assert!(matches!(
            live.admit(|| Arc::clone(&slots).try_acquire_owned().ok(), prompt("b")),
            Ok(Admitted::Start { .. })
        ));
    }

    #[test]
    fn a_finished_turn_is_not_running_before_its_runner_takes_the_next_prompt() {
        let live = session();
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let _ = live.admit(|| Arc::clone(&slots).try_acquire_owned().ok(), prompt("a"));
        let _ = live.admit(|| None, prompt("b"));
        assert!(live.state().0);
        live.end_turn();
        assert!(!live.state().0, "the turn has ended");
        let mut slot = Arc::clone(&slots).try_acquire_owned().ok();
        assert!(matches!(live.next(&mut slot), Next::Run(..)));
        assert!(live.state().0, "the next turn runs");
    }
}
