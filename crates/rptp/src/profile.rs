use crate::bmca::{
    BestForeignRecord, BestMasterClockAlgorithm, GrandMasterTrackingBmca, ListeningBmca,
    ForeignClockRecords, ParentTrackingBmca, QualificationTimeoutPolicy,
};
use crate::e2e::{DelayCycle, EndToEndDelayMechanism};
use crate::initializing::InitializingPort;
use crate::listening::ListeningPort;
use crate::master::{AnnounceCycle, MasterPort, SyncCycle};
use crate::message::SystemMessage;
use crate::port::{AnnounceReceiptTimeout, Port, Timeout};
use crate::portstate::PortState;
use crate::premaster::PreMasterPort;
use crate::slave::SlavePort;
use crate::time::{Duration, LogInterval};
use crate::uncalibrated::UncalibratedPort;

pub struct PortProfile {
    announce_receipt_timeout_interval: Duration,
    log_announce_interval: LogInterval,
    log_sync_interval: LogInterval,
    log_min_delay_request_interval: LogInterval,
}

impl PortProfile {
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
    pub(crate) fn log_min_delay_request_interval(&self) -> LogInterval {
        self.log_min_delay_request_interval
    }

    pub(crate) fn initializing<P: Port, S: ForeignClockRecords>(
        self,
        port: P,
        bmca: BestMasterClockAlgorithm,
        best_foreign: BestForeignRecord<S>,
    ) -> PortState<P, S> {
        PortState::Initializing(InitializingPort::new(
            port,
            bmca,
            best_foreign,
            self,
        ))
    }

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
}
