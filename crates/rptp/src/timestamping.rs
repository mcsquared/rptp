//! Transmission (egress) timestamping boundary.
//!
//! IEEE 1588 distinguishes between:
//! - **ingress timestamps**: taken when an event message is received, and
//! - **egress timestamps**: taken when an event message is transmitted.
//!
//! In `rptp`, ingress timestamps are provided directly to the domain via
//! [`crate::port::PortIngress::process_event_message`]. Egress timestamps are asynchronous: the
//! port sends an event message and later receives timestamp feedback once infrastructure has
//! obtained the actual transmit timestamp (e.g. from hardware timestamping).
//!
//! [`TxTimestamping`] is the minimal hook used by [`crate::port::Port::send_event`]:
//! - on successful event transmission, `rptp` calls [`TxTimestamping::stamp_egress`] with the
//!   domain [`EventMessage`] that was sent, and
//! - infrastructure is expected to feed back a corresponding
//!   [`crate::message::SystemMessage::Timestamp`](crate::message::SystemMessage) containing a
//!   [`crate::message::TimestampMessage`].
//!
//! Timestamp feedback is used by port states to complete collaborations that require an egress
//! timestamp, such as:
//! - producing a FollowUp for a previously transmitted two-step Sync, and
//! - computing delay/offset terms from DelayReq egress timestamps.

use crate::message::EventMessage;

/// Infrastructure hook for obtaining egress timestamps for transmitted event messages.
///
/// Implementations typically schedule or request a timestamp from the underlying network
/// interface and then inject a [`crate::message::SystemMessage::Timestamp`](crate::message::SystemMessage)
/// back into the port state machine once available.
pub trait TxTimestamping {
    /// Request/record egress timestamping for a successfully sent event message.
    ///
    /// `msg` is the domain event message that was transmitted. It is included so the timestamp
    /// feedback can carry enough information to be handled appropriately by the current port
    /// state (e.g. a two-step Sync timestamp results in a FollowUp; a DelayReq timestamp is used
    /// to compute delay/offset terms).
    fn stamp_egress(&self, msg: EventMessage);
}

impl<Tx: TxTimestamping> TxTimestamping for &Tx {
    fn stamp_egress(&self, msg: EventMessage) {
        (*self).stamp_egress(msg)
    }
}
