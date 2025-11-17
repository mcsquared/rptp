use crate::bmca::Bmca;
use crate::log::Log;
use crate::port::Port;
use crate::portstate::PortState;

pub struct InitializingPort<P: Port, B: Bmca, L: Log> {
    port: P,
    bmca: B,
    log: L,
}

impl<P: Port, B: Bmca, L: Log> InitializingPort<P, B, L> {
    pub fn new(port: P, bmca: B, log: L) -> Self {
        Self { port, bmca, log }
    }

    pub fn to_listening(self) -> PortState<P, B, L> {
        PortState::listening(self.port, self.bmca, self.log)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bmca::{FullBmca, LocalClockDS};
    use crate::clock::{FakeClock, LocalClock};
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::NoopLog;
    use crate::message::SystemMessage;
    use crate::port::test_support::{FakePort, FakeTimerHost};
    use crate::port::{DomainNumber, DomainPort, PortNumber};
    use crate::portstate::StateTransition;

    #[test]
    fn initializing_port_to_listening_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let mut initializing = PortState::Initializing(InitializingPort::new(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        ));

        let transition = initializing.dispatch_system(SystemMessage::Initialized);

        assert!(matches!(transition, Some(StateTransition::ToListening)));
    }
}
