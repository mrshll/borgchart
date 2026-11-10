//! A test-only cap on brg's fixed waits.
//!
//! Some waits are fixed periods that a test of the path past them would sit
//! out in full: the guarded wake's lock, render and submit waits, `brg
//! send`'s paste and submit waits, and the grace before KILL. In a build with the `test-support` feature, a process
//! started with [`ENV_CAP_MS`] (milliseconds, above 0) caps each at that, and
//! [`cap_for_tests`] does the same in-process (for library tests, which can't
//! safely set the environment). Builds without the feature ignore both.

use std::time::Duration;

pub const ENV_CAP_MS: &str = "BORGCHART_TEST_WAIT_CAP_MS";

#[cfg(feature = "test-support")]
static CAP_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Cap this process's fixed waits at `cap`.
#[cfg(feature = "test-support")]
pub fn cap_for_tests(cap: Duration) {
    let ms = u64::try_from(cap.as_millis()).unwrap_or(u64::MAX);
    CAP_MS.store(ms, std::sync::atomic::Ordering::Relaxed);
}

/// `real`, or the test cap when one is set and shorter.
pub fn wait(real: Duration) -> Duration {
    #[cfg(feature = "test-support")]
    {
        let ms = match CAP_MS.load(std::sync::atomic::Ordering::Relaxed) {
            0 => std::env::var(ENV_CAP_MS)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            ms => ms,
        };
        if ms > 0 {
            return real.min(Duration::from_millis(ms));
        }
    }
    real
}
