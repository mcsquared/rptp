use crate::bmca::{
    BestForeignRecord, BestMasterClockAlgorithm, ListeningBmca, SortedForeignClockRecords,
};
use crate::log::PortEvent;
use crate::port::Port;
use crate::portstate::{PortProfile, PortState};

pub struct InitializingPort<P: Port, S: SortedForeignClockRecords> {
    port: P,
    bmca: BestMasterClockAlgorithm,
    sorted_foreign_clock_records: S,
    profile: PortProfile,
}

impl<P: Port, S: SortedForeignClockRecords> InitializingPort<P, S> {
    pub(crate) fn new(
        port: P,
        bmca: BestMasterClockAlgorithm,
        sorted_foreign_clock_records: S,
        profile: PortProfile,
    ) -> Self {
        port.log(PortEvent::Static("Become InitializingPort"));

        Self {
            port,
            bmca,
            sorted_foreign_clock_records,
            profile,
        }
    }

    pub(crate) fn initialized(self) -> PortState<P, S> {
        self.port.log(PortEvent::Initialized);

        let bmca = ListeningBmca::new(
            self.bmca,
            BestForeignRecord::new(self.sorted_foreign_clock_records),
        );

        self.profile.listening(self.port, bmca)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                NoopPortLog,
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            BestMasterClockAlgorithm::new(*local_clock.default_ds()),
            SortedForeignClockRecordsVec::new(),
            PortProfile::default(),
        );

        let listening = initializing.initialized();

        assert!(matches!(listening, PortState::Listening(_)));
    }
}
