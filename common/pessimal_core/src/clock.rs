//! Time as a port.
//!
//! Reading the wall clock is I/O. The domain takes a [`Clock`] so liveness and alert evaluation
//! are deterministic under test.

use chrono::{DateTime, Utc};

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

/// The real clock. Adapters use this; tests do not.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A clock pinned to an instant, for tests.
#[derive(Debug, Clone)]
pub struct FixedClock(DateTime<Utc>);

impl FixedClock {
    #[must_use]
    pub fn new(at: DateTime<Utc>) -> Self {
        Self(at)
    }

    pub fn set(&mut self, at: DateTime<Utc>) {
        self.0 = at;
    }
}

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}
