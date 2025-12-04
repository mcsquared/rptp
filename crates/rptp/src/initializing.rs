use crate::bmca::Bmca;
use crate::log::{PortEvent, PortLog};
use crate::port::{Port, PortTimingPolicy};
use crate::portstate::PortState;

pub struct InitializingPort<P: Port, B: Bmca, L: PortLog> {
    port: P,
    bmca: B,
    log: L,
    timing_policy: PortTimingPolicy,
}

impl<P: Port, B: Bmca, L: PortLog> InitializingPort<P, B, L> {
    pub fn new(port: P, bmca: B, log: L, timing_policy: PortTimingPolicy) -> Self {
        log.port_event(PortEvent::Static("Become InitializingPort"));

        Self {
            port,
            bmca,
            log,
            timing_policy,
        }
    }

    pub fn initialized(self) -> PortState<P, B, L> {
        self.log.port_event(PortEvent::Initialized);
        PortState::listening(self.port, self.bmca, self.log, self.timing_policy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bmca::{DefaultDS, IncrementalBmca};
    use crate::clock::{LocalClock, StepsRemoved};
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::{NOOP_CLOCK_METRICS, NoopPortLog};
    use crate::message::SystemMessage;
    use crate::port::{DomainNumber, DomainPort, PortNumber};
    use crate::portstate::{PortState, StateDecision};
    use crate::test_support::{FakeClock, FakePort, FakeTimerHost, FakeTimestamping};

    #[test]
    fn initializing_port_to_listening_transition() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
            &NOOP_CLOCK_METRICS,
        );
        let mut initializing = PortState::Initializing(InitializingPort::new(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            PortTimingPolicy::default(),
        ));

        let transition = initializing.dispatch_system(SystemMessage::Initialized);

        assert!(matches!(transition, Some(StateDecision::Initialized)));
    }
}
