//! Port state: Faulty.
//!
//! This module models the `FAULTY` state of the IEEE 1588 port state machine.
//!
//! At the current stage, FAULTY is intentionally minimal:
//! - it is a quiescent state (no message handling / no periodic sending),
//! - it preserves the ownership of the minimal set of port collaborators required to
//!   re-initialize the port when the fault is cleared.

use crate::bmca::{BestForeignRecord, BestMasterClockAlgorithm, ForeignClockRecords};
use crate::log::PortEvent;
use crate::port::Port;
use crate::portstate::PortState;
use crate::profile::PortProfile;

/// Port role for the `FAULTY` state.
///
/// This state is entered upon fault detection (e.g., send failure or explicit fault condition).
/// While in this state, the port does not send or process any messages. It preserves ownership
/// of the underlying [`Port`] to allow for logging and recovery into initialization when the fault
/// is cleared.
pub struct FaultyPort<'a, P: Port, S: ForeignClockRecords> {
    port: P,
    bmca: BestMasterClockAlgorithm<'a>,
    best_foreign: BestForeignRecord<S>,
    profile: PortProfile,
}

impl<'a, P: Port, S: ForeignClockRecords> FaultyPort<'a, P, S> {
    pub(crate) fn new(
        port: P,
        bmca: BestMasterClockAlgorithm<'a>,
        best_foreign: BestForeignRecord<S>,
        profile: PortProfile,
    ) -> Self {
        port.log(PortEvent::Static("Become FaultyPort"));

        Self {
            port,
            bmca,
            best_foreign,
            profile,
        }
    }

    pub(crate) fn fault_cleared(self) -> PortState<'a, P, S> {
        self.profile
            .initializing(self.port, self.bmca, self.best_foreign)
    }
}
