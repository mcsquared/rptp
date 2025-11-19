use std::time::Duration;

use crate::bmca::Bmca;
use crate::log::Log;
use crate::master::{AnnounceCycle, MasterPort, SyncCycle};
use crate::message::SystemMessage;
use crate::port::Port;

pub struct PreMasterPort<P: Port, B: Bmca, L: Log> {
    port: P,
    bmca: B,
    _qualification_timeout: P::Timeout,
    log: L,
}

impl<P: Port, B: Bmca, L: Log> PreMasterPort<P, B, L> {
    pub fn new(port: P, bmca: B, _qualification_timeout: P::Timeout, log: L) -> Self {
        Self {
            port,
            bmca,
            _qualification_timeout,
            log,
        }
    }

    pub fn qualified(self) -> MasterPort<P, B, L> {
        let announce_send_timeout = self
            .port
            .timeout(SystemMessage::AnnounceSendTimeout, Duration::from_secs(0));
        let announce_cycle = AnnounceCycle::new(0.into(), announce_send_timeout);
        let sync_timeout = self
            .port
            .timeout(SystemMessage::SyncTimeout, Duration::from_secs(0));
        let sync_cycle = SyncCycle::new(0.into(), sync_timeout);

        MasterPort::new(self.port, self.bmca, announce_cycle, sync_cycle, self.log)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use crate::bmca::{FullBmca, LocalClockDS};
    use crate::clock::{FakeClock, LocalClock};
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::NoopLog;
    use crate::message::SystemMessage;
    use crate::port::test_support::{FakePort, FakeTimerHost};
    use crate::port::{DomainNumber, DomainPort, PortNumber};
    use crate::portstate::PortState;
    use crate::portstate::StateDecision;

    #[test]
    fn pre_master_port_schedules_qualification_timeout() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            &timer_host,
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let qualification_timeout =
            domain_port.timeout(SystemMessage::QualificationTimeout, Duration::from_secs(5));

        let _ = PreMasterPort::new(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            qualification_timeout,
            NoopLog,
        );

        let messages = timer_host.take_system_messages();
        assert!(messages.contains(&SystemMessage::QualificationTimeout));
    }

    #[test]
    fn pre_master_port_to_master_transition_on_qualification_timeout() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let qualification_timeout =
            domain_port.timeout(SystemMessage::QualificationTimeout, Duration::from_secs(5));

        let mut pre_master = PortState::PreMaster(PreMasterPort::new(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            qualification_timeout,
            NoopLog,
        ));

        let transition = pre_master.dispatch_system(SystemMessage::QualificationTimeout);

        assert!(matches!(
            transition,
            Some(StateDecision::QualificationTimeoutExpired)
        ));
    }
}
