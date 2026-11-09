use tokio::sync::{Mutex, MutexGuard};

// A Tokio mutex so async tests can hold the guard across `.await`.
static LOCK: Mutex<()> = Mutex::const_new(());

/// For plain `#[test]` functions. Panics if called inside a Tokio runtime.
pub(crate) fn lock() -> MutexGuard<'static, ()> {
    LOCK.blocking_lock()
}

/// For async tests.
pub(crate) async fn lock_async() -> MutexGuard<'static, ()> {
    LOCK.lock().await
}
