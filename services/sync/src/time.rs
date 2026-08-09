//! Injectable time, so presence expiry can be tested deterministically without
//! sleeping.
//!
//! [`SystemClock`] reads the real monotonic clock; [`ManualClock`] is advanced
//! under test control. Presence stores `Instant`s read from whichever clock the
//! room is configured with.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A source of monotonic time.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

/// Real wall-clock-backed monotonic time.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// A manually-advanced clock for tests. Starts at the real `Instant::now()` of
/// construction and only moves when [`ManualClock::advance`] is called.
#[derive(Debug)]
pub struct ManualClock {
    inner: Mutex<Instant>,
}

impl Default for ManualClock {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Instant::now()),
        }
    }
}

impl ManualClock {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance the clock forward by `dur`. Panics only if the result would
    /// overflow the monotonic range, which never happens in tests.
    pub fn advance(&self, dur: Duration) {
        let mut t = self.inner.lock().expect("manual clock poisoned");
        *t = t.checked_add(dur).expect("clock overflow");
    }

    /// Read the current virtual time.
    pub fn instant(&self) -> Instant {
        *self.inner.lock().expect("manual clock poisoned")
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.instant()
    }
}
