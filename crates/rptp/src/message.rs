//! PTP message ingress, parsing, and domain message representations.
//!
//! This module is the domain boundary between *raw bytes on the wire* and *typed domain messages*
//! dispatched into ports.
//!
//! ## Ingress flow
//!
//! [`MessageIngress`] performs minimal validation (PTPv2 header checks, length checks) and then:
//! - parses the header to determine the PTP domain number and source port identity, and
//! - parses the payload into an [`EventMessage`] or [`GeneralMessage`],
//! - dispatches the result into a [`PortMap`] (infrastructure-provided domain → port mapping).
//!
//! ## Event vs. general messages and time inputs
//!
//! PTP distinguishes between:
//! - **event messages** (e.g. Sync, DelayReq) that are timestamped at ingress/egress, and
//! - **general messages** (e.g. Announce, FollowUp, DelayResp) that are not.
//!
//! `rptp` mirrors that separation in its ingress API:
//! - [`MessageIngress::receive_event`] requires an ingress [`TimeStamp`] provided by infrastructure.
//! - [`MessageIngress::receive_general`] requires a local monotonic [`Instant`] used for
//!   qualification windows (e.g. Announce-based BMCA tracking).
//!
//! This module currently covers a subset of PTPv2 message types used by the core model; unsupported
//! message types are reported as [`ProtocolError`].

use crate::{
    bmca::{BestForeignSnapshot, Bmca, ClockDS},
    clock::TimeScale,
    port::{PortIdentity, PortMap},
    result::{ParseError, ProtocolError, Result},
    time::{Instant, LogMessageInterval, TimeInterval, TimeStamp},
    wire::{
        AnnouncePayload, ControlField, DelayResponsePayload, FinalizedBuffer, FollowUpPayload,
        LengthCheckedMessage, MessageBuffer, MessageFlags, MessageHeader, MessageType, SyncPayload,
        UnvalidatedMessage,
    },
};

/// Entry point for ingesting received PTP datagrams into the domain.
///
/// Infrastructure owns socket I/O and timestamping. It delivers received datagram bytes to this
/// type, which parses them into domain messages and dispatches them into the appropriate port via
/// a [`PortMap`].
///
/// Use [`receive_event`](Self::receive_event) for event messages (Sync/DelayReq/…) and
/// [`receive_general`](Self::receive_general) for general messages (Announce/FollowUp/DelayResp/…).
pub struct MessageIngress<'a, PM: PortMap> {
    ports: &'a mut PM,
}

impl<'a, PM: PortMap> MessageIngress<'a, PM> {
    /// Create a new ingress wrapper that dispatches to `ports`.
    pub fn new(ports: &'a mut PM) -> Self {
        Self { ports }
    }

    /// Parse and dispatch a received event message.
    ///
    /// `timestamp` is the ingress PTP timestamp provided by infrastructure for this datagram.
    pub fn receive_event(&mut self, buf: &[u8], timestamp: TimeStamp) -> Result<()> {
        let length_checked = UnvalidatedMessage::new(buf).length_checked_v2()?;
        DomainMessage::new(length_checked).dispatch_event(self.ports, timestamp)
    }

    /// Parse and dispatch a received general message.
    ///
    /// `now` is a local monotonic time instant used by the domain for time-window logic (e.g.
    /// foreign master qualification).
    pub fn receive_general(&mut self, buf: &[u8], now: Instant) -> Result<()> {
        let length_checked = UnvalidatedMessage::new(buf).length_checked_v2()?;
        DomainMessage::new(length_checked).dispatch_general(self.ports, now)
    }
}

/// A message wrapper that carries domain context for dispatch.
///
/// This type is deliberately named *DomainMessage* because it literally represents
/// “a message in a domain”: it is a parsed PTP message header plus the ability to dispatch the
/// message to the correct in-domain port via a [`PortMap`].
///
/// The mapping from domain number → port is provided by infrastructure; the domain core only
/// depends on the [`PortMap`] interface.
struct DomainMessage<'a> {
    header: MessageHeader<'a>,
}

impl<'a> DomainMessage<'a> {
    fn new(length_checked: LengthCheckedMessage<'a>) -> Self {
        Self {
            header: MessageHeader::new(length_checked),
        }
    }

    fn dispatch_event(self, ports: &mut impl PortMap, timestamp: TimeStamp) -> Result<()> {
        let domain_number = self.header.domain_number();
        let port = ports.port_by_domain(domain_number)?;
        let source_port_identity = self.header.source_port_identity();
        let msg = EventMessage::new(
            self.header.message_type()?,
            self.header.sequence_id(),
            self.header.flags(),
            self.header.log_message_interval(),
            self.header.payload(),
        )?;
        port.process_event_message(source_port_identity, msg, timestamp);

        Ok(())
    }

    fn dispatch_general(self, ports: &mut impl PortMap, now: Instant) -> Result<()> {
        let domain_number = self.header.domain_number();
        let port = ports.port_by_domain(domain_number)?;
        let source_port_identity = self.header.source_port_identity();
        let msg = GeneralMessage::new(
            self.header.message_type()?,
            self.header.sequence_id(),
            self.header.flags(),
            self.header.log_message_interval(),
            self.header.payload(),
        )?;
        port.process_general_message(source_port_identity, msg, now);

        Ok(())
    }
}

/// A parsed event message (timestamped message class).
///
/// This represents the subset of PTPv2 event messages that the core currently processes.
/// Unknown/unsupported message types are rejected during parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventMessage {
    DelayReq(DelayRequestMessage),
    OneStepSync(OneStepSyncMessage),
    TwoStepSync(TwoStepSyncMessage),
}

/// A parsed general message (non-timestamped message class).
///
/// This represents the subset of PTPv2 general messages that the core currently processes.
/// Unknown/unsupported message types are rejected during parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneralMessage {
    Announce(AnnounceMessage),
    DelayResp(DelayResponseMessage),
    FollowUp(FollowUpMessage),
}

/// A system/internal message delivered to ports by infrastructure.
///
/// These messages are not received from the network. They represent:
/// - scheduled timeouts (Announce send/receipt, DelayReq cycle, Sync cycle, qualification timeout),
/// - initialization completion (`Initialized`), and
/// - timestamp feedback (`Timestamp`) for previously sent event messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemMessage {
    AnnounceSendTimeout,
    DelayRequestTimeout,
    SyncTimeout,
    Timestamp(TimestampMessage),
    Initialized,
    FaultCleared,
    AnnounceReceiptTimeout,
    QualificationTimeout,
    StateDecisionEvent(crate::bmca::BestForeignSnapshot),
}

impl EventMessage {
    pub(crate) fn new(
        msg_type: MessageType,
        sequence_id: SequenceId,
        flags: MessageFlags,
        log_message_interval: LogMessageInterval,
        payload: &[u8],
    ) -> Result<Self> {
        match msg_type {
            MessageType::Sync => {
                if flags.contains(MessageFlags::TWO_STEP) {
                    Ok(Self::TwoStepSync(TwoStepSyncMessage::new(
                        sequence_id,
                        log_message_interval,
                    )))
                } else {
                    Ok(Self::OneStepSync(OneStepSyncMessage::new(
                        sequence_id,
                        log_message_interval,
                        SyncPayload::new(payload).origin_timestamp()?,
                    )))
                }
            }
            MessageType::DelayRequest => Ok(Self::DelayReq(DelayRequestMessage::new(sequence_id))),
            _ => Err(ProtocolError::UnknownMessageType(msg_type.to_nibble()).into()),
        }
    }

    pub(crate) fn to_wire<'a>(self, buf: &'a mut MessageBuffer) -> FinalizedBuffer<'a> {
        match self {
            EventMessage::DelayReq(msg) => msg.to_wire(buf),
            EventMessage::OneStepSync(msg) => msg.to_wire(buf),
            EventMessage::TwoStepSync(msg) => msg.to_wire(buf),
        }
    }
}

impl GeneralMessage {
    pub(crate) fn new(
        msg_type: MessageType,
        sequence_id: SequenceId,
        flags: MessageFlags,
        log_message_interval: LogMessageInterval,
        payload: &[u8],
    ) -> Result<Self> {
        match msg_type {
            MessageType::Announce => {
                let announce_payload = AnnouncePayload::new(payload);
                Ok(Self::Announce(AnnounceMessage::new(
                    sequence_id,
                    log_message_interval,
                    announce_payload.foreign_clock_ds()?,
                    if flags.contains(MessageFlags::PTP_TIMESCALE) {
                        TimeScale::Ptp
                    } else {
                        TimeScale::Arb
                    },
                )))
            }
            MessageType::FollowUp => Ok(Self::FollowUp(FollowUpMessage::new(
                sequence_id,
                log_message_interval,
                FollowUpPayload::new(payload).precise_origin_timestamp()?,
            ))),
            MessageType::DelayResponse => {
                let payload = DelayResponsePayload::new(payload);
                Ok(Self::DelayResp(DelayResponseMessage::new(
                    sequence_id,
                    log_message_interval,
                    payload.receive_timestamp()?,
                    payload.requesting_port_identity()?,
                )))
            }
            _ => Err(ProtocolError::UnknownMessageType(msg_type.to_nibble()).into()),
        }
    }

    pub(crate) fn to_wire<'a>(self, buf: &'a mut MessageBuffer) -> FinalizedBuffer<'a> {
        match self {
            GeneralMessage::Announce(msg) => msg.to_wire(buf),
            GeneralMessage::DelayResp(msg) => msg.to_wire(buf),
            GeneralMessage::FollowUp(msg) => msg.to_wire(buf),
        }
    }
}

/// Sequence identifier used to match related messages (e.g. Sync ↔ FollowUp, DelayReq ↔ DelayResp).
///
/// PTP sequence ids are 16-bit values that wrap on overflow. The domain uses this wrapper to keep
/// that behaviour explicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequenceId {
    id: u16,
}

impl SequenceId {
    pub(crate) fn new(id: u16) -> Self {
        Self { id }
    }

    #[cfg(test)]
    pub(crate) fn follows(&self, previous: SequenceId) -> bool {
        self.id.wrapping_sub(previous.id) == 1
    }

    pub(crate) fn next(&self) -> Self {
        Self {
            id: self.id.wrapping_add(1),
        }
    }

    pub(crate) fn to_be_bytes(self) -> [u8; 2] {
        self.id.to_be_bytes()
    }
}

impl From<u16> for SequenceId {
    fn from(id: u16) -> Self {
        Self::new(id)
    }
}

impl TryFrom<&[u8]> for SequenceId {
    type Error = crate::result::Error;

    fn try_from(buf: &[u8]) -> Result<Self> {
        let id = u16::from_be_bytes(
            buf.get(0..2)
                .ok_or(ParseError::PayloadTooShort {
                    field: "SequenceId",
                    expected: 2,
                    found: buf.len(),
                })?
                .try_into()
                .map_err(|_| ParseError::PayloadTooShort {
                    field: "SequenceId",
                    expected: 2,
                    found: buf.len(),
                })?,
        );
        Ok(Self::new(id))
    }
}

/// An Announce message, as used for BMCA input.
///
/// `rptp` models Announce primarily as a carrier of the foreign clock dataset (`ClockDS`) and the
/// sender's announced interval. Port states feed Announce messages into BMCA via [`feed_bmca`](Self::feed_bmca).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnnounceMessage {
    sequence_id: SequenceId,
    log_message_interval: LogMessageInterval,
    foreign_clock_ds: ClockDS,
    ptp_timescale: TimeScale,
}

impl AnnounceMessage {
    pub(crate) fn new(
        sequence_id: SequenceId,
        log_message_interval: LogMessageInterval,
        foreign_clock_ds: ClockDS,
        ptp_timescale: TimeScale,
    ) -> Self {
        Self {
            sequence_id,
            log_message_interval,
            foreign_clock_ds,
            ptp_timescale,
        }
    }

    pub(crate) fn feed_bmca(
        self,
        bmca: &mut impl Bmca,
        source_port_identity: PortIdentity,
        now: Instant,
    ) -> Option<BestForeignSnapshot> {
        if let Some(log_interval) = self.log_message_interval.log_interval() {
            bmca.consider(
                source_port_identity,
                self.foreign_clock_ds,
                log_interval,
                now,
            )
        } else {
            None
        }
    }

    pub(crate) fn to_wire<'a>(self, buf: &'a mut MessageBuffer) -> FinalizedBuffer<'a> {
        let ptp_timescale_flag = match self.ptp_timescale {
            TimeScale::Ptp => MessageFlags::PTP_TIMESCALE,
            TimeScale::Arb => MessageFlags::empty(),
        };

        let mut payload = buf
            .with_message_type(MessageType::Announce, ControlField::Other)
            .with_flags(ptp_timescale_flag)
            .with_sequence_id(self.sequence_id)
            .with_log_message_interval(self.log_message_interval)
            .payload();

        let payload_buf = payload.buf();
        payload_buf[13..29].copy_from_slice(&self.foreign_clock_ds.to_wire());

        payload.finalize(30)
    }
}

/// A one-step Sync message.
///
/// In one-step mode, the origin timestamp is carried directly in the Sync payload.
///
/// ## Behaviour and collaboration
///
/// This type is not treated as a passive DTO. It provides behaviour that higher-level domain
/// objects collaborate with:
/// - [`master_slave_offset`](Self::master_slave_offset) derives the master→slave offset estimate
///   when paired with an ingress timestamp provided by the receiver.
/// - [`EndToEndDelayMechanism`](crate::e2e::EndToEndDelayMechanism) uses that estimate as one half
///   of a servo sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OneStepSyncMessage {
    sequence_id: SequenceId,
    log_message_interval: LogMessageInterval,
    origin_timestamp: TimeStamp,
}

impl OneStepSyncMessage {
    pub(crate) fn new(
        sequence_id: SequenceId,
        log_message_interval: LogMessageInterval,
        origin_timestamp: TimeStamp,
    ) -> Self {
        Self {
            sequence_id,
            log_message_interval,
            origin_timestamp,
        }
    }

    /// Derive the master→slave offset estimate for this Sync.
    ///
    /// `ingress_timestamp` is the event ingress timestamp observed by the receiver for this Sync.
    /// The result is used as the Sync-side contribution to a servo sample (see
    /// `crate::e2e::EndToEndDelayMechanism`).
    pub(crate) fn master_slave_offset(&self, ingress_timestamp: TimeStamp) -> TimeInterval {
        ingress_timestamp - self.origin_timestamp
    }

    pub(crate) fn to_wire<'a>(self, buf: &'a mut MessageBuffer) -> FinalizedBuffer<'a> {
        let mut payload = buf
            .with_message_type(MessageType::Sync, ControlField::Sync)
            .with_flags(MessageFlags::empty())
            .with_sequence_id(self.sequence_id)
            .with_log_message_interval(self.log_message_interval)
            .payload();

        let payload_buf = payload.buf();
        payload_buf[..10].copy_from_slice(&self.origin_timestamp.to_wire());

        payload.finalize(10)
    }
}

/// A two-step Sync message.
///
/// In two-step mode, the Sync message is followed by a matching [`FollowUpMessage`] carrying the
/// precise origin timestamp. The two are matched by [`SequenceId`].
///
/// ## Behaviour and collaboration
///
/// This message is one half of a two-step exchange:
/// - `TwoStepSyncMessage` is the event message (timestamped at ingress by the receiver).
/// - [`FollowUpMessage`] is the general message that carries the origin timestamp.
///
/// The two are combined to derive the master→slave offset estimate via
/// [`FollowUpMessage::master_slave_offset`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TwoStepSyncMessage {
    sequence_id: SequenceId,
    log_message_interval: LogMessageInterval,
}

impl TwoStepSyncMessage {
    pub(crate) fn new(sequence_id: SequenceId, log_message_interval: LogMessageInterval) -> Self {
        Self {
            sequence_id,
            log_message_interval,
        }
    }

    /// Create the matching FollowUp message for this Sync.
    ///
    /// On the master side, infrastructure typically provides the precise origin timestamp (often
    /// derived from the Sync egress timestamp). The returned FollowUp carries the same
    /// [`SequenceId`] so that receivers can match it to the corresponding two-step Sync.
    pub(crate) fn follow_up(self, precise_origin_timestamp: TimeStamp) -> FollowUpMessage {
        FollowUpMessage::new(
            self.sequence_id,
            self.log_message_interval,
            precise_origin_timestamp,
        )
    }

    pub(crate) fn to_wire<'a>(self, buf: &'a mut MessageBuffer) -> FinalizedBuffer<'a> {
        let payload = buf
            .with_message_type(MessageType::Sync, ControlField::Sync)
            .with_flags(MessageFlags::TWO_STEP)
            .with_sequence_id(self.sequence_id)
            .with_log_message_interval(self.log_message_interval)
            .payload();

        payload.finalize(10)
    }
}

/// A FollowUp message carrying the precise origin timestamp for a two-step Sync.
///
/// ## Behaviour and collaboration
///
/// This type is designed for collaboration with [`TwoStepSyncMessage`]:
/// - [`master_slave_offset`](Self::master_slave_offset) matches by [`SequenceId`] and derives the
///   master→slave offset estimate when paired with the receiver-observed Sync ingress timestamp.
///
/// `crate::e2e::EndToEndDelayMechanism` uses that offset estimate as one half of a servo sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FollowUpMessage {
    sequence_id: SequenceId,
    log_message_interval: LogMessageInterval,
    precise_origin_timestamp: TimeStamp,
}

impl FollowUpMessage {
    pub(crate) fn new(
        sequence_id: SequenceId,
        log_message_interval: LogMessageInterval,
        precise_origin_timestamp: TimeStamp,
    ) -> Self {
        Self {
            sequence_id,
            log_message_interval,
            precise_origin_timestamp,
        }
    }

    /// Derive the master→slave offset estimate for a two-step Sync/FollowUp pair.
    ///
    /// Returns `None` if `sync` and `self` do not belong to the same two-step exchange (sequence id
    /// mismatch).
    pub(crate) fn master_slave_offset(
        &self,
        sync: TwoStepSyncMessage,
        sync_ingress_timestamp: TimeStamp,
    ) -> Option<TimeInterval> {
        if self.sequence_id == sync.sequence_id {
            Some(sync_ingress_timestamp - self.precise_origin_timestamp)
        } else {
            None
        }
    }

    pub(crate) fn to_wire<'a>(self, buf: &'a mut MessageBuffer) -> FinalizedBuffer<'a> {
        let mut payload = buf
            .with_message_type(MessageType::FollowUp, ControlField::FollowUp)
            .with_flags(MessageFlags::empty())
            .with_sequence_id(self.sequence_id)
            .with_log_message_interval(self.log_message_interval)
            .payload();

        let payload_buf = payload.buf();
        payload_buf[..10].copy_from_slice(&self.precise_origin_timestamp.to_wire());

        payload.finalize(10)
    }
}

/// A DelayReq event message.
///
/// The slave sends DelayReq and later records its egress timestamp (provided by infrastructure) to
/// compute the slave→master offset when a matching [`DelayResponseMessage`] arrives.
///
/// ## Behaviour and collaboration
///
/// This type participates in the delay measurement exchange:
/// - On the master side, [`response`](Self::response) generates the corresponding
///   [`DelayResponseMessage`].
/// - On the slave side, the egress timestamp of this message (reported via
///   [`SystemMessage::Timestamp`]) is paired with a matching [`DelayResponseMessage`] via
///   [`DelayResponseMessage::slave_master_offset`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelayRequestMessage {
    sequence_id: SequenceId,
}

impl DelayRequestMessage {
    pub(crate) fn new(sequence_id: SequenceId) -> Self {
        Self { sequence_id }
    }

    /// Generate the corresponding DelayResp for this DelayReq.
    ///
    /// This models the master-side collaboration: given the master's receive timestamp for the
    /// DelayReq (`receive_timestamp`) and the requester's identity, produce the matching response
    /// message carrying the same [`SequenceId`].
    pub(crate) fn response(
        self,
        log_message_interval: LogMessageInterval,
        receive_timestamp: TimeStamp,
        requesting_port_identity: PortIdentity,
    ) -> DelayResponseMessage {
        DelayResponseMessage::new(
            self.sequence_id,
            log_message_interval,
            receive_timestamp,
            requesting_port_identity,
        )
    }

    pub(crate) fn to_wire<'a>(self, buf: &'a mut MessageBuffer) -> FinalizedBuffer<'a> {
        let payload = buf
            .with_message_type(MessageType::DelayRequest, ControlField::DelayRequest)
            .with_flags(MessageFlags::empty())
            .with_sequence_id(self.sequence_id)
            .with_log_message_interval(LogMessageInterval::unspecified())
            .payload();

        payload.finalize(10)
    }
}

/// A DelayResp general message.
///
/// Carries the master's receive timestamp for a previously sent DelayReq and the identity of the
/// requesting port.
///
/// ## Behaviour and collaboration
///
/// This type is designed to collaborate with [`DelayRequestMessage`] and the receiver-provided
/// DelayReq egress timestamp:
/// - [`slave_master_offset`](Self::slave_master_offset) matches by [`SequenceId`] and derives the
///   slave→master offset estimate for the delay exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelayResponseMessage {
    sequence_id: SequenceId,
    log_message_interval: LogMessageInterval,
    receive_timestamp: TimeStamp,
    requesting_port_identity: PortIdentity,
}

impl DelayResponseMessage {
    pub(crate) fn new(
        sequence_id: SequenceId,
        log_message_interval: LogMessageInterval,
        receive_timestamp: TimeStamp,
        requesting_port_identity: PortIdentity,
    ) -> Self {
        Self {
            sequence_id,
            log_message_interval,
            receive_timestamp,
            requesting_port_identity,
        }
    }

    /// Derive the slave→master offset estimate for a DelayReq/DelayResp pair.
    ///
    /// Returns `None` if `delay_req` and `self` do not belong to the same exchange (sequence id
    /// mismatch).
    ///
    /// `delay_req_egress_timestamp` is the event egress timestamp for the DelayReq as observed by
    /// the sender (slave) and reported back by infrastructure.
    pub(crate) fn slave_master_offset(
        &self,
        delay_req: DelayRequestMessage,
        delay_req_egress_timestamp: TimeStamp,
    ) -> Option<TimeInterval> {
        if self.sequence_id == delay_req.sequence_id {
            Some(self.receive_timestamp - delay_req_egress_timestamp)
        } else {
            None
        }
    }

    pub(crate) fn to_wire<'a>(self, buf: &'a mut MessageBuffer) -> FinalizedBuffer<'a> {
        let mut payload = buf
            .with_message_type(MessageType::DelayResponse, ControlField::DelayResponse)
            .with_flags(MessageFlags::empty())
            .with_sequence_id(self.sequence_id)
            .with_log_message_interval(self.log_message_interval)
            .payload();

        let payload_buf = payload.buf();
        payload_buf[..10].copy_from_slice(&self.receive_timestamp.to_wire());
        payload_buf[10..20].copy_from_slice(&self.requesting_port_identity.to_wire());

        payload.finalize(20)
    }
}

/// Timestamp feedback for a previously sent event message.
///
/// Infrastructure emits this as a [`SystemMessage::Timestamp`] once an egress timestamp is known
/// for an event transmission (two-step Sync, DelayReq).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimestampMessage {
    pub event_msg: EventMessage,
    pub egress_timestamp: TimeStamp,
}

impl TimestampMessage {
    /// Create a new timestamp feedback message.
    pub fn new(event_msg: EventMessage, egress_timestamp: TimeStamp) -> Self {
        Self {
            event_msg,
            egress_timestamp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::wire::UnvalidatedMessage;

    use crate::bmca::{Priority1, Priority2};
    use crate::clock::{ClockAccuracy, ClockClass, ClockIdentity, ClockQuality, StepsRemoved};
    use crate::port::{DomainNumber, PortIdentity, PortIngress, PortNumber};
    use crate::time::LogMessageInterval;
    use crate::wire::{PtpVersion, TransportSpecific};

    struct CapturingPort {
        last_event: Option<(PortIdentity, EventMessage, TimeStamp)>,
        last_general: Option<(PortIdentity, GeneralMessage, Instant)>,
        last_system: Option<SystemMessage>,
    }

    impl CapturingPort {
        fn new() -> Self {
            Self {
                last_event: None,
                last_general: None,
                last_system: None,
            }
        }
    }

    impl PortIngress for CapturingPort {
        fn process_event_message(
            &mut self,
            source_port_identity: PortIdentity,
            msg: EventMessage,
            timestamp: TimeStamp,
        ) {
            self.last_event = Some((source_port_identity, msg, timestamp));
        }

        fn process_general_message(
            &mut self,
            source_port_identity: PortIdentity,
            msg: GeneralMessage,
            now: Instant,
        ) {
            self.last_general = Some((source_port_identity, msg, now));
        }

        fn process_system_message(&mut self, msg: SystemMessage) {
            self.last_system = Some(msg);
        }
    }

    struct CapturingPortMap {
        domain: DomainNumber,
        port: CapturingPort,
    }

    impl CapturingPortMap {
        fn new(domain: DomainNumber) -> Self {
            Self {
                domain,
                port: CapturingPort::new(),
            }
        }
    }

    impl PortMap for CapturingPortMap {
        fn port_by_domain(&mut self, domain_number: DomainNumber) -> Result<&mut dyn PortIngress> {
            if self.domain == domain_number {
                Ok(&mut self.port)
            } else {
                Err(ProtocolError::DomainNotFound(domain_number.as_u8()).into())
            }
        }
    }

    #[test]
    fn announce_message_wire_roundtrip() {
        let announce = AnnounceMessage::new(
            42.into(),
            LogMessageInterval::new(0x7F),
            ClockDS::new(
                ClockIdentity::new(&[0; 8]),
                Priority1::new(127),
                Priority2::new(127),
                ClockQuality::new(ClockClass::Default, ClockAccuracy::Within250ns, 0xFFFF),
                StepsRemoved::new(42),
            ),
            TimeScale::Ptp,
        );

        let mut buf = MessageBuffer::new(
            TransportSpecific,
            PtpVersion::V2,
            DomainNumber::new(0),
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1)),
        );
        let wire = announce.to_wire(&mut buf);
        let mut ports = CapturingPortMap::new(DomainNumber::new(0));
        let now = Instant::from_nanos(42);
        MessageIngress::new(&mut ports)
            .receive_general(wire.as_ref(), now)
            .unwrap();

        let (source_port_identity, parsed, captured_now) = ports.port.last_general.unwrap();
        assert_eq!(
            source_port_identity,
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1))
        );
        assert_eq!(captured_now, now);
        assert_eq!(parsed, GeneralMessage::Announce(announce));
    }

    #[test]
    fn one_step_sync_message_wire_roundtrip() {
        let sync =
            OneStepSyncMessage::new(42.into(), LogMessageInterval::new(5), TimeStamp::new(1, 2));
        let mut buf = MessageBuffer::new(
            TransportSpecific,
            PtpVersion::V2,
            DomainNumber::new(0),
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1)),
        );
        let wire = sync.to_wire(&mut buf);
        let mut ports = CapturingPortMap::new(DomainNumber::new(0));
        let timestamp = TimeStamp::new(5, 6);
        MessageIngress::new(&mut ports)
            .receive_event(wire.as_ref(), timestamp)
            .unwrap();

        let (source_port_identity, parsed, captured_timestamp) = ports.port.last_event.unwrap();
        assert_eq!(
            source_port_identity,
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1))
        );
        assert_eq!(captured_timestamp, timestamp);
        assert_eq!(parsed, EventMessage::OneStepSync(sync));
    }

    #[test]
    fn two_step_sync_message_wire_roundtrip() {
        let sync = TwoStepSyncMessage::new(42.into(), LogMessageInterval::new(7));
        let mut buf = MessageBuffer::new(
            TransportSpecific,
            PtpVersion::V2,
            DomainNumber::new(0),
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1)),
        );
        let wire = sync.to_wire(&mut buf);
        let mut ports = CapturingPortMap::new(DomainNumber::new(0));
        let timestamp = TimeStamp::new(5, 6);
        MessageIngress::new(&mut ports)
            .receive_event(wire.as_ref(), timestamp)
            .unwrap();

        let (source_port_identity, parsed, captured_timestamp) = ports.port.last_event.unwrap();
        assert_eq!(
            source_port_identity,
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1))
        );
        assert_eq!(captured_timestamp, timestamp);
        assert_eq!(parsed, EventMessage::TwoStepSync(sync));
    }

    #[test]
    fn follow_up_message_wire_roundtrip() {
        let follow_up =
            FollowUpMessage::new(42.into(), LogMessageInterval::new(3), TimeStamp::new(1, 2));
        let mut buf = MessageBuffer::new(
            TransportSpecific,
            PtpVersion::V2,
            DomainNumber::new(0),
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1)),
        );
        let wire = follow_up.to_wire(&mut buf);
        let mut ports = CapturingPortMap::new(DomainNumber::new(0));
        let now = Instant::from_nanos(42);
        MessageIngress::new(&mut ports)
            .receive_general(wire.as_ref(), now)
            .unwrap();

        let (source_port_identity, parsed, captured_now) = ports.port.last_general.unwrap();
        assert_eq!(
            source_port_identity,
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1))
        );
        assert_eq!(captured_now, now);
        assert_eq!(parsed, GeneralMessage::FollowUp(follow_up));
    }

    #[test]
    fn delay_request_message_wire_roundtrip() {
        let delay_req = DelayRequestMessage::new(42.into());
        let mut buf = MessageBuffer::new(
            TransportSpecific,
            PtpVersion::V2,
            DomainNumber::new(0),
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1)),
        );
        let wire = delay_req.to_wire(&mut buf);
        let mut ports = CapturingPortMap::new(DomainNumber::new(0));
        let timestamp = TimeStamp::new(5, 6);
        MessageIngress::new(&mut ports)
            .receive_event(wire.as_ref(), timestamp)
            .unwrap();

        let (source_port_identity, parsed, captured_timestamp) = ports.port.last_event.unwrap();
        assert_eq!(
            source_port_identity,
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1))
        );
        assert_eq!(captured_timestamp, timestamp);
        assert_eq!(parsed, EventMessage::DelayReq(delay_req));
    }

    #[test]
    fn delay_response_message_wire_roundtrip() {
        let requesting_port_identity = PortIdentity::new(
            ClockIdentity::new(&[1, 2, 3, 4, 5, 6, 7, 8]),
            PortNumber::new(9),
        );
        let delay_resp = DelayResponseMessage::new(
            42.into(),
            LogMessageInterval::new(-2),
            TimeStamp::new(1, 2),
            requesting_port_identity,
        );
        let mut buf = MessageBuffer::new(
            TransportSpecific,
            PtpVersion::V2,
            DomainNumber::new(0),
            PortIdentity::fake(),
        );
        let wire = delay_resp.to_wire(&mut buf);
        let mut ports = CapturingPortMap::new(DomainNumber::new(0));
        let now = Instant::from_nanos(42);
        MessageIngress::new(&mut ports)
            .receive_general(wire.as_ref(), now)
            .unwrap();

        let (source_port_identity, parsed, captured_now) = ports.port.last_general.unwrap();
        assert_eq!(source_port_identity, PortIdentity::fake());
        assert_eq!(captured_now, now);
        assert_eq!(parsed, GeneralMessage::DelayResp(delay_resp));
    }

    #[test]
    fn event_message_new_reports_short_sync_payload() {
        let payload = [0u8; 5]; // less than 10 bytes required for origin_timestamp

        let res = EventMessage::new(
            MessageType::Sync,
            1.into(),
            MessageFlags::empty(),
            LogMessageInterval::new(3),
            &payload,
        );

        assert_eq!(
            res,
            Err(ParseError::PayloadTooShort {
                field: "Sync.origin_timestamp",
                expected: 10,
                found: 5,
            }
            .into())
        );
    }

    #[test]
    fn general_message_new_reports_short_announce_payload() {
        let payload = [0u8; 20]; // shorter than 13 + 16 required bytes

        let res = GeneralMessage::new(
            MessageType::Announce,
            1.into(),
            MessageFlags::empty(),
            LogMessageInterval::new(0),
            &payload,
        );

        assert_eq!(
            res,
            Err(ParseError::PayloadTooShort {
                field: "Announce.foreign_clock_ds",
                expected: 16,
                found: payload.len().saturating_sub(13),
            }
            .into())
        );
    }

    #[test]
    fn general_message_new_reports_short_delay_response_timestamp() {
        let payload = [0u8; 5]; // less than 10 bytes required for receive_timestamp

        let res = GeneralMessage::new(
            MessageType::DelayResponse,
            1.into(),
            MessageFlags::empty(),
            LogMessageInterval::new(0),
            &payload,
        );

        assert_eq!(
            res,
            Err(ParseError::PayloadTooShort {
                field: "DelayResponse.receive_timestamp",
                expected: 10,
                found: 5,
            }
            .into())
        );
    }

    #[test]
    fn general_message_new_reports_short_delay_response_requesting_port_identity() {
        let payload = [0u8; 15]; // 10 bytes for timestamp, 5 bytes for identity (short of 10)

        let res = GeneralMessage::new(
            MessageType::DelayResponse,
            1.into(),
            MessageFlags::empty(),
            LogMessageInterval::new(0),
            &payload,
        );

        assert_eq!(
            res,
            Err(ParseError::PayloadTooShort {
                field: "DelayResponse.requesting_port_identity",
                expected: 10,
                found: 5,
            }
            .into())
        );
    }

    #[test]
    fn sequence_id_try_from_reports_short_payload() {
        let buf = [0u8; 1];

        let res = SequenceId::try_from(&buf[..]);

        assert_eq!(
            res,
            Err(ParseError::PayloadTooShort {
                field: "SequenceId",
                expected: 2,
                found: 1,
            }
            .into())
        );
    }

    #[test]
    fn domain_message_dispatch_event_propagates_domain_not_found() {
        struct FailingPortMap;

        impl PortMap for FailingPortMap {
            fn port_by_domain(
                &mut self,
                domain_number: DomainNumber,
            ) -> Result<&mut dyn PortIngress> {
                Err(ProtocolError::DomainNotFound(domain_number.as_u8()).into())
            }
        }

        let mut buf = [0u8; 34 + 10];
        buf[1] = PtpVersion::V2.as_u8();
        let total_len = buf.len() as u16;
        buf[2..4].copy_from_slice(&total_len.to_be_bytes());

        // Message type nibble: Sync and some domain number
        buf[0] |= MessageType::Sync.to_nibble();
        buf[4] = 42;

        let length_checked = UnvalidatedMessage::new(&buf)
            .length_checked_v2()
            .expect("length check must succeed");
        let domain_msg = DomainMessage::new(length_checked);

        let mut ports = FailingPortMap;
        let res = domain_msg.dispatch_event(&mut ports, TimeStamp::new(0, 0));

        assert_eq!(res, Err(ProtocolError::DomainNotFound(42).into()));
    }

    #[test]
    fn twostep_sync_message_produces_follow_up() {
        let sync = TwoStepSyncMessage::new(42.into(), LogMessageInterval::new(9));
        let follow_up = sync.follow_up(TimeStamp::new(4, 0));

        assert_eq!(
            follow_up,
            FollowUpMessage::new(42.into(), LogMessageInterval::new(9), TimeStamp::new(4, 0))
        );
    }

    #[test]
    fn follow_up_message_produces_master_slave_offset() {
        let sync = TwoStepSyncMessage::new(42.into(), LogMessageInterval::new(11));
        let follow_up =
            FollowUpMessage::new(42.into(), LogMessageInterval::new(11), TimeStamp::new(4, 0));

        let sync_ingress_timestamp = TimeStamp::new(5, 0);
        let offset = follow_up.master_slave_offset(sync, sync_ingress_timestamp);

        assert_eq!(offset, Some(TimeInterval::new(1, 0)));
    }

    #[test]
    fn follow_up_message_with_different_sequence_id_produces_no_master_slave_offset() {
        let sync = TwoStepSyncMessage::new(42.into(), LogMessageInterval::new(11));
        let follow_up =
            FollowUpMessage::new(43.into(), LogMessageInterval::new(11), TimeStamp::new(4, 0));

        let sync_ingress_timestamp = TimeStamp::new(5, 0);
        let offset = follow_up.master_slave_offset(sync, sync_ingress_timestamp);

        assert_eq!(offset, None);
    }

    #[test]
    fn delay_response_produces_slave_master_offset() {
        let delay_req = DelayRequestMessage::new(42.into());
        let delay_resp = DelayResponseMessage::new(
            42.into(),
            LogMessageInterval::new(5),
            TimeStamp::new(5, 0),
            PortIdentity::fake(),
        );

        let delay_req_egress_timestamp = TimeStamp::new(4, 0);
        let offset = delay_resp.slave_master_offset(delay_req, delay_req_egress_timestamp);

        assert_eq!(offset, Some(TimeInterval::new(1, 0)));
    }

    #[test]
    fn delay_response_with_different_sequence_id_produces_no_slave_master_offset() {
        let delay_req = DelayRequestMessage::new(42.into());
        let delay_resp = DelayResponseMessage::new(
            43.into(),
            LogMessageInterval::new(-1),
            TimeStamp::new(5, 0),
            PortIdentity::fake(),
        );

        let delay_req_egress_timestamp = TimeStamp::new(4, 0);
        let offset = delay_resp.slave_master_offset(delay_req, delay_req_egress_timestamp);

        assert_eq!(offset, None);
    }

    #[test]
    fn delay_request_message_produces_delay_response_message() {
        let delay_req = DelayRequestMessage::new(42.into());
        let delay_resp = delay_req.response(
            LogMessageInterval::new(1),
            TimeStamp::new(4, 0),
            PortIdentity::fake(),
        );

        assert_eq!(
            delay_resp,
            DelayResponseMessage::new(
                42.into(),
                LogMessageInterval::new(1),
                TimeStamp::new(4, 0),
                PortIdentity::fake()
            )
        );
    }

    #[test]
    fn sequence_id_follows_next_for_all_values() {
        for id in 0u16..=u16::MAX {
            let a: SequenceId = id.into();
            let b = a.next();
            assert!(b.follows(a), "next({id}) should follow {id}");
            assert!(!a.follows(a));
        }
    }

    #[test]
    fn sequence_id_roundtrip_to_from_be_bytes() {
        let samples = [0u16, 1, 2, 7, 42, 255, 256, 1024, 4096, 32767, 65534, 65535];
        for &id in &samples {
            let sid: SequenceId = id.into();
            let bytes = sid.to_be_bytes();
            let parsed = SequenceId::try_from(&bytes[..]).unwrap();
            assert_eq!(parsed, sid);
        }
    }

    #[test]
    fn sequence_id_does_not_follow_on_gap() {
        let a: SequenceId = 10u16.into();
        let c: SequenceId = 12u16.into();
        assert!(!c.follows(a));
    }

    #[test]
    fn sequence_id_wraps() {
        let a: SequenceId = u16::MAX.into();
        let b: SequenceId = 0u16.into();
        assert!(b.follows(a));
        assert_eq!(a.next(), b);
    }
}
