use std::time::Duration;

use crate::bmca::{Bmca, BmcaRecommendation};
use crate::listening::ListeningPort;
use crate::log::Log;
use crate::master::{AnnounceCycle, MasterPort, SyncCycle};
use crate::message::{AnnounceMessage, SystemMessage};
use crate::port::{ParentPortIdentity, Port, PortIdentity, Timeout};
use crate::portstate::StateTransition;
use crate::slave::{DelayCycle, SlavePort};

pub struct UncalibratedPort<P: Port, B: Bmca, L: Log> {
    port: P,
    bmca: B,
    announce_receipt_timeout: P::Timeout,
    log: L,
}

impl<P: Port, B: Bmca, L: Log> UncalibratedPort<P, B, L> {
    pub fn new(port: P, bmca: B, announce_receipt_timeout: P::Timeout, log: L) -> Self {
        Self {
            port,
            bmca,
            announce_receipt_timeout,
            log,
        }
    }

    pub fn process_announce(
        &mut self,
        msg: AnnounceMessage,
        source_port_identity: PortIdentity,
    ) -> Option<StateTransition> {
        self.log.message_received("Announce");
        self.announce_receipt_timeout
            .restart(Duration::from_secs(5));
        self.bmca.consider(source_port_identity, msg);

        match self.bmca.recommendation(self.port.local_clock()) {
            BmcaRecommendation::Master => Some(StateTransition::ToPreMaster),
            BmcaRecommendation::Slave(parent) => Some(StateTransition::ToSlave(parent)),
            BmcaRecommendation::Undecided => Some(StateTransition::ToListening),
        }
    }

    pub fn to_slave(self, parent_port_identity: ParentPortIdentity) -> SlavePort<P, B, L> {
        let delay_cycle = DelayCycle::new(
            0.into(),
            self.port
                .timeout(SystemMessage::DelayRequestTimeout, Duration::from_secs(0)),
        );
        SlavePort::new(
            self.port,
            self.bmca,
            parent_port_identity,
            self.announce_receipt_timeout,
            delay_cycle,
            self.log,
        )
    }

    pub fn to_listening(self) -> ListeningPort<P, B, L> {
        ListeningPort::new(
            self.port,
            self.bmca,
            self.announce_receipt_timeout,
            self.log,
        )
    }

    pub fn to_master(self) -> MasterPort<P, B, L> {
        let announce_send_timeout = self.port.timeout(
            SystemMessage::AnnounceSendTimeout,
            Duration::from_secs(0),
        );
        let announce_cycle = AnnounceCycle::new(0.into(), announce_send_timeout);
        let sync_timeout = self.port.timeout(SystemMessage::SyncTimeout, Duration::from_secs(0));
        let sync_cycle = SyncCycle::new(0.into(), sync_timeout);

        MasterPort::new(self.port, self.bmca, announce_cycle, sync_cycle, self.log)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bmca::{ForeignClockDS, ForeignClockRecord, FullBmca, LocalClockDS};
    use crate::clock::{FakeClock, LocalClock};
    use crate::portstate::PortState;
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::NoopLog;
    use crate::port::test_support::{FakePort, FakeTimerHost};
    use crate::port::{DomainNumber, DomainPort, PortNumber};

    #[test]
    fn uncalibrated_port_to_slave_transition_on_following_announce() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let foreign_clock_ds = ForeignClockDS::high_grade_test_clock();
        let prior_records = [ForeignClockRecord::new(
            PortIdentity::fake(),
            AnnounceMessage::new(41.into(), foreign_clock_ds),
        )
        .with_resolved_clock(foreign_clock_ds)];
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let mut uncalibrated = UncalibratedPort::new(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::from_records(&prior_records)),
            announce_receipt_timeout,
            NoopLog,
        );

        let transition = uncalibrated.process_announce(
            AnnounceMessage::new(42.into(), foreign_clock_ds),
            PortIdentity::fake(),
        );

        assert!(matches!(transition, Some(StateTransition::ToSlave(_))));
    }

    #[test]
    fn uncalibrated_port_to_master_on_announce_receipt_timeout() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let mut uncalibrated = PortState::Uncalibrated(UncalibratedPort::new(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            announce_receipt_timeout,
            NoopLog,
        ));

        let transition = uncalibrated.dispatch_system(SystemMessage::AnnounceReceiptTimeout);

        assert!(matches!(transition, Some(StateTransition::ToMaster)));
    }
}
