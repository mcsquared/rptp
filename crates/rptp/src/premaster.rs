use crate::bmca::Bmca;
use crate::log::PortLog;
use crate::port::{Port, PortTimingPolicy};
use crate::portstate::PortState;

pub struct PreMasterPort<P: Port, B: Bmca, L: PortLog> {
    port: P,
    bmca: B,
    _qualification_timeout: P::Timeout,
    log: L,
    timing_policy: PortTimingPolicy,
}

impl<P: Port, B: Bmca, L: PortLog> PreMasterPort<P, B, L> {
    pub fn new(
        port: P,
        bmca: B,
        _qualification_timeout: P::Timeout,
        log: L,
        timing_policy: PortTimingPolicy,
    ) -> Self {
        Self {
            port,
            bmca,
            _qualification_timeout,
            log,
            timing_policy,
        }
    }

    pub fn qualified(self) -> PortState<P, B, L> {
        self.log
            .state_transition("Pre-Master", "Master", "Port has qualified as Master");

        PortState::master(self.port, self.bmca, self.log, self.timing_policy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use crate::bmca::{DefaultDS, FullBmca};
    use crate::clock::{FakeClock, LocalClock, StepsRemoved};
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::NoopPortLog;
    use crate::message::SystemMessage;
    use crate::port::test_support::{FakePort, FakeTimerHost};
    use crate::port::{DomainNumber, DomainPort, PortNumber};
    use crate::portstate::PortState;
    use crate::portstate::StateDecision;

    #[test]
    fn pre_master_port_schedules_qualification_timeout() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );
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
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let messages = timer_host.take_system_messages();
        assert!(messages.contains(&SystemMessage::QualificationTimeout));
    }

    #[test]
    fn pre_master_port_to_master_transition_on_qualification_timeout() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );
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
            NoopPortLog,
            PortTimingPolicy::default(),
        ));

        let transition = pre_master.dispatch_system(SystemMessage::QualificationTimeout);

        assert!(matches!(
            transition,
            Some(StateDecision::QualificationTimeoutExpired)
        ));
    }
}
