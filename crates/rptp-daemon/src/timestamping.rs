//! Daemon-side timestamping adapters.
//!
//! The `rptp` core distinguishes between:
//! - ingress timestamps (when an event message is received), and
//! - egress timestamps (when an event message is transmitted).
//!
//! In the daemon, both are typically derived from the same clock implementation:
//! - [`ClockRxTimestamping`] provides ingress timestamps to [`TokioPortsLoop`](crate::node::TokioPortsLoop),
//! - [`ClockTxTimestamping`] implements [`rptp::timestamping::TxTimestamping`] and converts a
//!   successful event send into a [`SystemMessage::Timestamp`] injected back into the domain.
//!
//! This is a minimal integration suitable for experiments and tests. More realistic setups may
//! use hardware timestamping where egress timestamps arrive asynchronously from the NIC/driver.

use tokio::sync::mpsc;

use rptp::{
    clock::Clock,
    message::{EventMessage, SystemMessage, TimestampMessage},
    port::DomainNumber,
    time::TimeStamp,
    timestamping::TxTimestamping,
};

/// Ingress timestamping boundary for the daemon IO loop.
///
/// `rptp` expects event datagrams to be delivered with an ingress timestamp. The daemon uses this
/// trait so it can plug in different timestamp sources (virtual clock, system clock, hardware
/// timestamping, ...).
pub trait RxTimestamping {
    /// Return the ingress timestamp to attach to the next received event message.
    fn ingress_stamp(&self) -> TimeStamp;
}

/// Egress timestamping adapter that synthesizes timestamp feedback from a local clock.
///
/// When an event message is sent successfully, the `rptp` domain calls
/// [`TxTimestamping::stamp_egress`]. This implementation immediately samples `clock.now()` and
/// sends a [`SystemMessage::Timestamp`] into the daemon's system message channel.
pub struct ClockTxTimestamping<C: Clock> {
    clock: C,
    system_tx: mpsc::UnboundedSender<(DomainNumber, SystemMessage)>,
    domain: DomainNumber,
}

impl<C: Clock> ClockTxTimestamping<C> {
    /// Create a new egress timestamping adapter for `domain`.
    pub fn new(
        clock: C,
        system_tx: mpsc::UnboundedSender<(DomainNumber, SystemMessage)>,
        domain: DomainNumber,
    ) -> Self {
        Self {
            clock,
            system_tx,
            domain,
        }
    }
}

impl<C: Clock> TxTimestamping for ClockTxTimestamping<C> {
    fn stamp_egress(&self, msg: EventMessage) {
        let system_msg = SystemMessage::Timestamp(TimestampMessage::new(msg, self.clock.now()));
        let _ = self.system_tx.send((self.domain, system_msg));
    }
}

/// Ingress timestamping adapter that reads `clock.now()`.
///
/// The timestamp is used as the ingress timestamp for received event messages.
pub struct ClockRxTimestamping<C: Clock> {
    clock: C,
}

impl<C: Clock> ClockRxTimestamping<C> {
    /// Create a new ingress timestamping adapter.
    pub fn new(clock: C) -> Self {
        Self { clock }
    }
}

impl<C: Clock> RxTimestamping for ClockRxTimestamping<C> {
    fn ingress_stamp(&self) -> TimeStamp {
        self.clock.now()
    }
}
