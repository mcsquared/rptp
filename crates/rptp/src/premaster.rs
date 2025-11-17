use crate::bmca::Bmca;
use crate::log::Log;
use crate::port::Port;
use crate::portstate::PortState;

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

    pub fn to_master(self) -> PortState<P, B, L> {
        PortState::master(self.port, self.bmca, self.log)
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
    use crate::portstate::StateTransition;

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

        assert!(matches!(transition, Some(StateTransition::ToMaster)));
    }
}
