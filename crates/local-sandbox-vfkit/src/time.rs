//! Time seam for deterministic lifecycle tests.

use std::time::{SystemTime, UNIX_EPOCH};

/// Fakeable clock boundary.
pub(crate) trait Clock: Send + Sync {
    fn now_unix_ms(&self) -> u64;
}

/// System wall-clock implementation.
pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn now_unix_ms(&self) -> u64 {
        u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(u64::MAX)
    }
}
