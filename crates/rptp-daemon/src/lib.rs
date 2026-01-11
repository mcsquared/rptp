//! Tokio-based daemon infrastructure for `rptp`.
//!
//! The `rptp` workspace separates the protocol/domain core (`crates/rptp`) from the
//! host/IO-facing integration layer (`crates/rptp-daemon`).
//!
//! This crate provides a small set of building blocks typically used by:
//! - the `rptp-daemon` binary (`src/main.rs`), and
//! - other binaries/tests that want to embed the same components.
//!
//! Most of the domain behaviour lives in the `rptp` crate; this crate focuses on wiring:
//! networking, timestamp feedback, virtual clocks, and an `OrdinaryClock` node runtime.

/// Logging/tracing integration and log sinks for daemon components.
pub mod log;
/// UDP/network helpers used by the daemon runtime.
pub mod net;
/// High-level node runtime glue (wiring ports, tasks, and IO).
pub mod node;
/// Daemon-side assembly of the `rptp` ordinary clock.
pub mod ordinary;
/// Daemon-side egress timestamping integration.
pub mod timestamping;
/// Virtual clock implementations for experiments and tests.
pub mod virtualclock;

use std::fmt;
use std::time::Instant;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;

/// Tracing `FormatTime` implementation that prints process uptime with millisecond precision.
///
/// Output format example: `rptp[12.034s]`.
struct MillisecondUptime {
    start: Instant,
}

impl MillisecondUptime {
    /// Start an uptime timer from “now”.
    fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl FormatTime for MillisecondUptime {
    fn format_time(&self, w: &mut Writer<'_>) -> fmt::Result {
        let elapsed = self.start.elapsed();
        let secs = elapsed.as_secs();
        let millis = elapsed.subsec_millis();
        write!(w, "rptp[{}.{:03}s]", secs, millis)
    }
}

/// Install a default tracing subscriber for binaries that embed the daemon components.
///
/// This mirrors the daemon binary: honor `RUST_LOG` with a default of `info`,
/// emit to stdout, and ignore the error if a subscriber is already set (e.g. tests).
pub fn init_tracing() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(std::io::stdout)
        .with_target(false)
        .with_level(false)
        .with_timer(MillisecondUptime::new())
        .try_init();
}
