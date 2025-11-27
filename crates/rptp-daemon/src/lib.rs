pub mod clock;
pub mod log;
pub mod net;
pub mod node;
pub mod ordinary;

use std::fmt;
use std::time::Instant;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;

struct MillisecondUptime {
    start: Instant,
}

impl MillisecondUptime {
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
