//! Port profile configuration and state assembly.
//!
//! A [`PortProfile`] bundles timing- and interval-related configuration for a PTP port and
//! provides convenience constructors for building the initial [`PortState`] variants with the
//! corresponding timeouts/cycles set up.
//!
//! The profile is *consumed* (`self`) when building a state. Each concrete port state owns a copy
//! of the profile so it can use the same configuration consistently across transitions.
//!
//! This module is intentionally narrow in scope: it does not implement the state machine itself
//! (see [`crate::portstate`]); it only wires together collaborators and starts the required
//! timers.

use crate::bmca::{
    BestForeignRecord, BestMasterClockAlgorithm, ForeignClockRecords, GrandMasterTrackingBmca,
    ListeningBmca, ParentTrackingBmca, PassiveBmca, QualificationTimeoutPolicy,
};
use crate::e2e::{DelayCycle, EndToEndDelayMechanism};
use crate::faulty::FaultyPort;
use crate::initializing::InitializingPort;
use crate::listening::ListeningPort;
use crate::master::{AnnounceCycle, MasterPort, SyncCycle};
use crate::message::SystemMessage;
use crate::passive::PassivePort;
use crate::port::{AnnounceReceiptTimeout, Port, Timeout};
use crate::portstate::PortState;
use crate::premaster::PreMasterPort;
use crate::slave::SlavePort;
use crate::time::{Duration, LogInterval};
use crate::uncalibrated::UncalibratedPort;

/// Port timing configuration used by the domain state machine.
///
/// The profile controls how often the port sends periodic messages and how long it waits for
/// incoming Announce messages before timing out.
///
/// - `announce_receipt_timeout_interval`: wall-clock [`Duration`] used by the announce receipt
///   timer (the “announce receipt timeout interval” in IEEE 1588 terminology).
/// - `log_announce_interval`: log2 interval used to schedule Announce transmissions when acting
///   as master (`logAnnounceInterval`).
/// - `log_sync_interval`: log2 interval used to schedule Sync transmissions when acting as master
///   (`logSyncInterval`).
/// - `log_min_delay_request_interval`: log2 interval used to schedule DelayReq transmissions when
///   acting as slave (`logMinDelayReqInterval`).
pub struct PortProfile {
    announce_receipt_timeout_interval: Duration,
    log_announce_interval: LogInterval,
    log_sync_interval: LogInterval,
    log_min_delay_request_interval: LogInterval,
}

impl PortProfile {
    /// Create a profile with explicit timing/interval values.
    ///
    /// This constructor is the primary way to configure a port’s periodic schedules and receipt
    /// timeouts from the outside. The values are later applied when constructing concrete port
    /// states via [`PortProfile::listening`], [`PortProfile::master`], [`PortProfile::slave`], etc.
    pub fn new(
        announce_receipt_timeout_interval: Duration,
        log_announce_interval: LogInterval,
        log_sync_interval: LogInterval,
        log_min_delay_request_interval: LogInterval,
    ) -> Self {
        Self {
            announce_receipt_timeout_interval,
            log_announce_interval,
            log_sync_interval,
            log_min_delay_request_interval,
        }
    }
}

impl Default for PortProfile {
    /// Default timing values suitable for tests and early bring-up.
    ///
    /// - announce receipt timeout: 5 seconds
    /// - announce/sync/delay request intervals: `LogInterval::new(0)` (1 second)
    fn default() -> Self {
        Self {
            announce_receipt_timeout_interval: Duration::from_secs(5),
            log_announce_interval: LogInterval::new(0),
            log_sync_interval: LogInterval::new(0),
            log_min_delay_request_interval: LogInterval::new(0),
        }
    }
}

impl PortProfile {
    /// Return the configured `logMinDelayReqInterval`.
    ///
    /// This is commonly embedded into outgoing Announce messages so other nodes can understand
    /// the slave’s expected delay-request pacing.
    pub(crate) fn log_min_delay_request_interval(&self) -> LogInterval {
        self.log_min_delay_request_interval
    }

    /// Construct the `INITIALIZING` state.
    ///
    /// This variant is used while infrastructure and BMCA bookkeeping are being brought up.
    pub(crate) fn initializing<P: Port, S: ForeignClockRecords>(
        self,
        port: P,
        bmca: BestMasterClockAlgorithm,
        best_foreign: BestForeignRecord<S>,
    ) -> PortState<P, S> {
        PortState::Initializing(InitializingPort::new(port, bmca, best_foreign, self))
    }

    /// Construct the `LISTENING` state and start the announce receipt timeout.
    ///
    /// The announce receipt timer is restarted immediately so the port can transition away from
    /// listening if no Announce messages arrive within the configured interval.
    pub(crate) fn listening<P: Port, S: ForeignClockRecords>(
        self,
        port: P,
        bmca: ListeningBmca<S>,
    ) -> PortState<P, S> {
        let announce_receipt_timeout = AnnounceReceiptTimeout::new(
            port.timeout(SystemMessage::AnnounceReceiptTimeout),
            self.announce_receipt_timeout_interval,
        );
        announce_receipt_timeout.restart();

        PortState::Listening(ListeningPort::new(
            port,
            bmca,
            announce_receipt_timeout,
            self,
        ))
    }

    /// Construct the `MASTER` state and start the periodic Announce/Sync schedules.
    ///
    /// Both cycles are started with an initial “send immediately” timeout (`0s`) so a newly
    /// elected master begins announcing and synchronizing without waiting for a full interval.
    pub(crate) fn master<P: Port, S: ForeignClockRecords>(
        self,
        port: P,
        bmca: GrandMasterTrackingBmca<S>,
    ) -> PortState<P, S> {
        let announce_send_timeout = port.timeout(SystemMessage::AnnounceSendTimeout);
        announce_send_timeout.restart(Duration::from_secs(0));
        let announce_cycle =
            AnnounceCycle::new(0.into(), announce_send_timeout, self.log_announce_interval);
        let sync_timeout = port.timeout(SystemMessage::SyncTimeout);
        sync_timeout.restart(Duration::from_secs(0));
        let sync_cycle = SyncCycle::new(0.into(), sync_timeout, self.log_sync_interval);

        PortState::Master(MasterPort::new(
            port,
            bmca,
            announce_cycle,
            sync_cycle,
            self,
        ))
    }

    /// Construct the `SLAVE` state and start the announce receipt timeout.
    ///
    /// The provided delay mechanism defines how DelayReq/DelayResp exchanges are scheduled and
    /// combined with Sync/FollowUp to yield offset measurements for the servo.
    pub(crate) fn slave<P: Port, S: ForeignClockRecords>(
        self,
        port: P,
        bmca: ParentTrackingBmca<S>,
        delay_mechanism: EndToEndDelayMechanism<P::Timeout>,
    ) -> PortState<P, S> {
        let announce_receipt_timeout = AnnounceReceiptTimeout::new(
            port.timeout(SystemMessage::AnnounceReceiptTimeout),
            self.announce_receipt_timeout_interval,
        );
        announce_receipt_timeout.restart();

        PortState::Slave(SlavePort::new(
            port,
            bmca,
            announce_receipt_timeout,
            delay_mechanism,
            self,
        ))
    }

    /// Construct the `PRE_MASTER` state and start the qualification timeout.
    ///
    /// The qualification timeout duration is derived from the profile’s announce interval and
    /// the provided [`QualificationTimeoutPolicy`].
    pub(crate) fn pre_master<P: Port, S: ForeignClockRecords>(
        self,
        port: P,
        bmca: GrandMasterTrackingBmca<S>,
        qualification_timeout_policy: QualificationTimeoutPolicy,
    ) -> PortState<P, S> {
        let qualification_timeout = port.timeout(SystemMessage::QualificationTimeout);
        qualification_timeout
            .restart(qualification_timeout_policy.duration(self.log_announce_interval));

        PortState::PreMaster(PreMasterPort::new(port, bmca, qualification_timeout, self))
    }

    /// Construct the `UNCALIBRATED` state.
    ///
    /// This state participates in offset measurement while the servo is not yet "locked".
    /// It starts both:
    /// - the announce receipt timeout (to detect loss of announces), and
    /// - the delay-request schedule (initially immediate) to begin collecting delay samples.
    pub(crate) fn uncalibrated<P: Port, S: ForeignClockRecords>(
        self,
        port: P,
        bmca: ParentTrackingBmca<S>,
    ) -> PortState<P, S> {
        let announce_receipt_timeout = AnnounceReceiptTimeout::new(
            port.timeout(SystemMessage::AnnounceReceiptTimeout),
            self.announce_receipt_timeout_interval,
        );
        announce_receipt_timeout.restart();

        let delay_timeout = port.timeout(SystemMessage::DelayRequestTimeout);
        delay_timeout.restart(Duration::from_secs(0));

        let delay_cycle =
            DelayCycle::new(0.into(), delay_timeout, self.log_min_delay_request_interval);

        PortState::Uncalibrated(UncalibratedPort::new(
            port,
            bmca,
            announce_receipt_timeout,
            EndToEndDelayMechanism::new(delay_cycle),
            self,
        ))
    }

    /// Construct the `FAULTY` state.
    ///
    /// This variant is used when the port has detected a fault condition and is not operational.
    pub(crate) fn faulty<P: Port, S: ForeignClockRecords>(
        self,
        port: P,
        bmca: BestMasterClockAlgorithm,
        best_foreign: BestForeignRecord<S>,
    ) -> PortState<P, S> {
        PortState::Faulty(FaultyPort::new(port, bmca, best_foreign, self))
    }

    /// Construct the `PASSIVE` state and start the announce receipt timeout.
    ///
    /// The announce receipt timer is restarted immediately so the port can transition away from
    /// passive if no Announce messages arrive within the configured interval.
    pub(crate) fn passive<P: Port, S: ForeignClockRecords>(
        self,
        port: P,
        bmca: PassiveBmca<S>,
    ) -> PortState<P, S> {
        let announce_receipt_timeout = AnnounceReceiptTimeout::new(
            port.timeout(SystemMessage::AnnounceReceiptTimeout),
            self.announce_receipt_timeout_interval,
        );
        announce_receipt_timeout.restart();

        PortState::Passive(PassivePort::new(port, bmca, announce_receipt_timeout, self))
    }
}
