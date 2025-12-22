use crate::bmca::Bmca;
use crate::log::{PortEvent, PortLog};
use crate::port::Port;
use crate::portstate::{PortProfile, PortState};

pub struct InitializingPort<P: Port, B: Bmca, L: PortLog> {
    port: P,
    bmca: B,
    log: L,
    profile: PortProfile,
}

impl<P: Port, B: Bmca, L: PortLog> InitializingPort<P, B, L> {
    pub(crate) fn new(port: P, bmca: B, log: L, profile: PortProfile) -> Self {
        log.port_event(PortEvent::Static("Become InitializingPort"));

        Self {
            port,
            bmca,
            log,
            profile,
        }
    }

    pub(crate) fn initialized(self) -> PortState<P, B, L> {
        self.log.port_event(PortEvent::Initialized);
        self.profile.listening(self.port, self.bmca, self.log)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bmca::IncrementalBmca;
    use crate::clock::LocalClock;
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::{NOOP_CLOCK_METRICS, NoopPortLog};
    use crate::port::{DomainNumber, DomainPort, PortNumber};
    use crate::portstate::PortState;
    use crate::servo::{Servo, SteppingServo};
    use crate::test_support::{
        FakeClock, FakePort, FakeTimerHost, FakeTimestamping, TestClockCatalog,
    };

    #[test]
    fn initializing_port_to_listening_transition() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            TestClockCatalog::default_mid_grade().default_ds(),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let physical_port = FakePort::new();
        let initializing = InitializingPort::new(
            DomainPort::new(
                &local_clock,
                &physical_port,
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            PortProfile::default(),
        );

        let listening = initializing.initialized();

        assert!(matches!(listening, PortState::Listening(_)));
    }
}
