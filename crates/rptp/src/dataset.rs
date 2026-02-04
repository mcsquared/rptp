//! BMCA comparison data set and wire format.
//!
//! This module defines the vocabulary used when comparing local and foreign clocks in the
//! Best Master Clock Algorithm (BMCA):
//!
//! - **[`ClockDS`]** – The data set describing a grandmaster candidate (identity,
//!   priority1, priority2, quality, steps_removed). It corresponds to the fields carried in
//!   Announce messages and used by BMCA when comparing clocks. [`ClockDS::compare`] encodes the
//!   BMCA ordering rules, returning either a strict result (`Better`/`Worse`) or indicating that a
//!   topology tie-break is required.
//! - **[`Priority1`]**, **[`Priority2`]**, **[`StepsRemoved`]** – Dataset field types used in
//!   comparisons and in the wire representation.
//!
//! The module also provides the 16-byte BMCA comparison tuple wire format ([`ClockDS::from_wire`],
//! [`ClockDS::to_wire`]) as used in PTP Announce payloads.
//!
//! The BMCA algorithm and roles that *use* these types live in [`crate::bmca`]. Foreign master
//! tracking and qualification (which hold and qualify [`ClockDS`] from Announce messages) live in
//! [`crate::foreign`].

use core::cmp::Ordering;
use core::ops::Range;

use crate::clock::{ClockIdentity, ClockQuality};
use crate::port::PortIdentity;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClockDSComparisonResult {
    Better,
    Worse,
    TopologyTieBreak(TopologyComparison),
}

/// Result of comparing two clock datasets with topology context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TopologyOrdering {
    /// Dataset A is better than B (not solely due to the topology tie-break).
    Better,
    /// Dataset A is better than B due to the topology tie-break.
    BetterByTopology,
    /// Dataset A is worse than B (not solely due to the topology tie-break).
    Worse,
    /// Dataset A is worse than B due to the topology tie-break.
    WorseByTopology,
    /// Error condition 1 (`error-1`): receiver equals sender (self-loop).
    Error1,
    /// Error condition 2 (`error-2`): identical topology (same sender, same receiver port).
    Error2,
}

impl TopologyOrdering {
    fn is_better_or_better_by_topology(&self) -> bool {
        matches!(
            self,
            TopologyOrdering::Better | TopologyOrdering::BetterByTopology
        )
    }

    fn is_better_by_topology(&self) -> bool {
        matches!(self, TopologyOrdering::BetterByTopology)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TopologyComparison {
    steps_removed_ordering: Ordering,
}

impl TopologyComparison {
    fn compare(&self, a: Topology, b: Topology) -> TopologyOrdering {
        a.compare(self.steps_removed_ordering, &b)
    }
}

/// A clock dataset bound to its IEEE 1588 topology tuple for comparison.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TopologyDS<'a> {
    ds: &'a ClockDS,
    topo: Topology<'a>,
}

impl<'a> TopologyDS<'a> {
    pub(crate) fn local(ds: &'a ClockDS, local_topo_id: &'a PortIdentity) -> Self {
        Self {
            ds,
            topo: Topology::local(local_topo_id),
        }
    }

    pub(crate) fn foreign(
        ds: &'a ClockDS,
        sender: &'a PortIdentity,
        receiver: &'a PortIdentity,
    ) -> Self {
        Self {
            ds,
            topo: Topology::foreign(sender, receiver),
        }
    }

    /// Return `true` if this dataset is better by the clock dataset comparison or by the topology
    /// tie-break.
    pub(crate) fn is_better_or_better_by_topology(self, other: TopologyDS<'a>) -> bool {
        match self.ds.compare(other.ds) {
            ClockDSComparisonResult::Better => true,
            ClockDSComparisonResult::Worse => false,
            ClockDSComparisonResult::TopologyTieBreak(tiebreak) => tiebreak
                .compare(self.topo, other.topo)
                .is_better_or_better_by_topology(),
        }
    }

    /// Return `true` only when this dataset is better than `other` *solely* due to the topology
    /// tie-break.
    ///
    /// This is used for the P2/M3 decision point in BMCA (IEEE 1588-2019 §9.3.3; Fig. 33).
    pub(crate) fn is_better_by_topology(self, other: TopologyDS<'a>) -> bool {
        match self.ds.compare(other.ds) {
            ClockDSComparisonResult::Better | ClockDSComparisonResult::Worse => false,
            ClockDSComparisonResult::TopologyTieBreak(tiebreak) => tiebreak
                .compare(self.topo, other.topo)
                .is_better_by_topology(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Topology<'a> {
    sender: &'a PortIdentity,
    receiver: &'a PortIdentity,
}

impl<'a> Topology<'a> {
    /// Construct the “self topology” used when comparing the local dataset `D_0`.
    ///
    /// IEEE 1588-2019 defines the topology tuple used during dataset comparison
    /// (sender/receiver port identities) in §9.3.4, Tbl. 29. For the local dataset, both the
    /// sender and receiver are the same `PortIdentity(clockIdentity, 0)`.
    fn local(local_topo_id: &'a PortIdentity) -> Self {
        Self {
            sender: local_topo_id,
            receiver: local_topo_id,
        }
    }

    /// Construct the topology tuple for a foreign dataset received on a port.
    ///
    /// IEEE 1588-2019 defines this shape in §9.3.3, Tbl. 29:
    /// - `sender` is the `sourcePortIdentity` from the received Announce,
    /// - `receiver` is the port identity of the local receiving port.
    fn foreign(sender: &'a PortIdentity, receiver: &'a PortIdentity) -> Self {
        Self { sender, receiver }
    }

    /// Compare this topology against `other` according to the BMCA topology tie-break rules.
    ///
    /// This implements the topology comparison phase of the BMCA as defined in IEEE 1588-2019
    /// §9.3.4; Fig. 35, excluding the stepsRemoved comparison (which is done prior to this, see
    /// `struct StepsRemoved`).
    fn compare(&self, steps_removed_ordering: Ordering, other: &Topology) -> TopologyOrdering {
        let sender_a = &self.sender;
        let receiver_a = &self.receiver;
        let sender_b = &other.sender;
        let receiver_b = &other.receiver;

        match steps_removed_ordering {
            Ordering::Less => match receiver_b.cmp(sender_b) {
                Ordering::Less => TopologyOrdering::Better,
                Ordering::Greater => TopologyOrdering::BetterByTopology,
                Ordering::Equal => TopologyOrdering::Error1,
            },
            Ordering::Greater => match receiver_a.cmp(sender_a) {
                Ordering::Less => TopologyOrdering::Worse,
                Ordering::Greater => TopologyOrdering::WorseByTopology,
                Ordering::Equal => TopologyOrdering::Error1,
            },
            Ordering::Equal => match sender_a.cmp(sender_b) {
                Ordering::Less => TopologyOrdering::BetterByTopology,
                Ordering::Greater => TopologyOrdering::WorseByTopology,
                Ordering::Equal => match receiver_a.compare_by_port_number(receiver_b) {
                    Ordering::Less => TopologyOrdering::BetterByTopology,
                    Ordering::Greater => TopologyOrdering::WorseByTopology,
                    Ordering::Equal => TopologyOrdering::Error2,
                },
            },
        }
    }
}

/// Data set describing a grandmaster candidate.
///
/// This corresponds closely to the fields carried in Announce messages and
/// used by the BMCA when comparing local and foreign clocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockDS {
    identity: ClockIdentity,
    priority1: Priority1,
    priority2: Priority2,
    quality: ClockQuality,
    steps_removed: StepsRemoved,
}

impl ClockDS {
    const PRIORITY1_OFFSET: usize = 0;
    const QUALITY_RANGE: Range<usize> = 1..5;
    const PRIORITY2_OFFSET: usize = 5;
    const IDENTITY_RANGE: Range<usize> = 6..14;
    const STEPS_REMOVED_OFFSET: Range<usize> = 14..16;

    pub fn new(
        identity: ClockIdentity,
        priority1: Priority1,
        priority2: Priority2,
        quality: ClockQuality,
        steps_removed: StepsRemoved,
    ) -> Self {
        Self {
            identity,
            priority1,
            priority2,
            quality,
            steps_removed,
        }
    }

    /// Return the clock identity of this dataset.
    pub fn identity(&self) -> &ClockIdentity {
        &self.identity
    }

    /// Return `true` if this dataset represents an authoritative clock.
    ///
    /// A clock is authoritative when its clock class is 1 to 127 inclusive, as denoted in
    /// the spec multiple times, but not named explicitly.
    ///
    /// See
    /// - IEEE 1588-2019 §9.3.3; Fig. 33
    pub(crate) fn is_authoritative(&self) -> bool {
        self.quality.is_authoritative()
    }

    /// Parse a `ClockDS` from the binary representation used on the
    /// wire (16‑byte BMCA comparison tuple).
    pub(crate) fn from_wire(buf: &[u8; 16]) -> Self {
        Self {
            identity: ClockIdentity::new(&[
                buf[Self::IDENTITY_RANGE.start],
                buf[Self::IDENTITY_RANGE.start + 1],
                buf[Self::IDENTITY_RANGE.start + 2],
                buf[Self::IDENTITY_RANGE.start + 3],
                buf[Self::IDENTITY_RANGE.start + 4],
                buf[Self::IDENTITY_RANGE.start + 5],
                buf[Self::IDENTITY_RANGE.start + 6],
                buf[Self::IDENTITY_RANGE.start + 7],
            ]),
            priority1: Priority1::new(buf[ClockDS::PRIORITY1_OFFSET]),
            priority2: Priority2::new(buf[ClockDS::PRIORITY2_OFFSET]),
            quality: ClockQuality::from_wire(&[
                buf[ClockDS::QUALITY_RANGE.start],
                buf[ClockDS::QUALITY_RANGE.start + 1],
                buf[ClockDS::QUALITY_RANGE.start + 2],
                buf[ClockDS::QUALITY_RANGE.start + 3],
            ]),
            steps_removed: StepsRemoved::new(u16::from_be_bytes([
                buf[ClockDS::STEPS_REMOVED_OFFSET.start],
                buf[ClockDS::STEPS_REMOVED_OFFSET.start + 1],
            ])),
        }
    }

    /// Compare this dataset against `other` according to the BMCA ordering rules according to
    /// IEEE 1588-2019 §9.3.4; Fig. 34.
    ///
    /// This returns a strict ordering (`Better`/`Worse`) when the datasets differ in priority,
    /// quality, priority2, or identity. If the datasets have the same grandmaster identity
    /// (`clockIdentity`), the comparison moves to the `stepsRemoved` + topology tie-break phase
    /// and returns `TopologyTieBreak(...)`.
    ///
    /// Note: the `stepsRemoved` + topology phase is only reached when the two datasets have the
    /// same grandmaster identity (`clockIdentity`). This matches the IEEE 1588 BMCA dataset
    /// comparison flow, where `stepsRemoved` is compared only for the same grandmaster.
    fn compare(&self, other: &ClockDS) -> ClockDSComparisonResult {
        if self.identity == other.identity {
            return self.steps_removed.compare(&other.steps_removed);
        }

        let a = (
            &self.priority1,
            &self.quality,
            &self.priority2,
            &self.identity,
        );
        let b = (
            &other.priority1,
            &other.quality,
            &other.priority2,
            &other.identity,
        );

        match a.cmp(&b) {
            Ordering::Less => ClockDSComparisonResult::Better,
            Ordering::Greater => ClockDSComparisonResult::Worse,
            Ordering::Equal => {
                unreachable!("datasets should at least differ in identity")
            }
        }
    }

    /// Serialize this data set into the 16‑byte BMCA comparison tuple
    /// representation.
    pub(crate) fn to_wire(self) -> [u8; 16] {
        let mut bytes = [0u8; 16];
        bytes[Self::PRIORITY1_OFFSET] = self.priority1.as_u8();
        bytes[Self::QUALITY_RANGE].copy_from_slice(&self.quality.to_wire());
        bytes[Self::PRIORITY2_OFFSET] = self.priority2.as_u8();
        bytes[Self::IDENTITY_RANGE].copy_from_slice(self.identity.as_bytes());
        bytes[Self::STEPS_REMOVED_OFFSET.start..Self::STEPS_REMOVED_OFFSET.end]
            .copy_from_slice(&self.steps_removed.to_be_bytes());
        bytes
    }
}

/// `defaultDS.priority1` as defined in IEEE 1588-2019.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Priority1(u8);

impl Priority1 {
    /// Create a new `Priority1` from a raw `u8` value.
    pub const fn new(value: u8) -> Self {
        Self(value)
    }

    /// Return the underlying `u8` value.
    pub(crate) fn as_u8(self) -> u8 {
        self.0
    }
}

impl From<u8> for Priority1 {
    fn from(value: u8) -> Self {
        Self::new(value)
    }
}

/// `defaultDS.priority2` as defined in IEEE 1588-2019.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Priority2(u8);

impl Priority2 {
    /// Create a new `Priority2` from a raw `u8` value.
    pub const fn new(value: u8) -> Self {
        Self(value)
    }

    /// Return the underlying `u8` value.
    pub(crate) fn as_u8(self) -> u8 {
        self.0
    }
}

impl From<u8> for Priority2 {
    fn from(value: u8) -> Self {
        Self::new(value)
    }
}

/// The PTP `stepsRemoved` dataset field.
///
/// This counts the number of communication paths between the local clock and the grandmaster. It
/// participates in BMCA comparisons and is carried in Announce messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct StepsRemoved(u16);

impl StepsRemoved {
    /// Create a `StepsRemoved` value from the raw counter.
    pub fn new(steps_removed: u16) -> Self {
        Self(steps_removed)
    }

    /// Compare this `StepsRemoved` against `other` according to the BMCA rules.
    ///
    /// This implementents the two first diamonds of the BMCA comparison flow in IEEE 1588-2019
    /// §9.3.4; Fig. 35.
    fn compare(&self, other: &StepsRemoved) -> ClockDSComparisonResult {
        if self.0.saturating_add(1) < other.0 {
            ClockDSComparisonResult::Better
        } else if self.0 > other.0.saturating_add(1) {
            ClockDSComparisonResult::Worse
        } else {
            ClockDSComparisonResult::TopologyTieBreak(TopologyComparison {
                steps_removed_ordering: self.0.cmp(&other.0),
            })
        }
    }

    pub(crate) fn as_u16(&self) -> u16 {
        self.0
    }

    pub(crate) fn to_be_bytes(self) -> [u8; 2] {
        self.0.to_be_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::TestClockDS;
    use crate::{clock::ClockIdentity, port::PortNumber};

    fn pid(clock_id: u8, port: u16) -> PortIdentity {
        PortIdentity::new(
            ClockIdentity::new(&[clock_id, 0, 0, 0, 0, 0, 0, 0]),
            PortNumber::new(port),
        )
    }

    #[test]
    fn clock_ds_ordering() {
        let high = TestClockDS::default_high_grade().dataset();
        let mid = TestClockDS::default_mid_grade().dataset();
        let low = TestClockDS::default_low_grade_slave_only().dataset();

        assert_eq!(high.compare(&mid), ClockDSComparisonResult::Better);
        assert_eq!(mid.compare(&high), ClockDSComparisonResult::Worse);

        assert_eq!(high.compare(&low), ClockDSComparisonResult::Better);
        assert_eq!(low.compare(&high), ClockDSComparisonResult::Worse);

        assert_eq!(mid.compare(&low), ClockDSComparisonResult::Better);
        assert_eq!(low.compare(&mid), ClockDSComparisonResult::Worse);
    }

    #[test]
    fn clock_ds_topology_tiebreak() {
        let c1 = TestClockDS::default_high_grade().dataset();
        let c2 = TestClockDS::default_high_grade().dataset();

        assert!(matches!(
            c1.compare(&c2),
            ClockDSComparisonResult::TopologyTieBreak(_)
        ));
        assert!(matches!(
            c2.compare(&c1),
            ClockDSComparisonResult::TopologyTieBreak(_)
        ));
        assert_eq!(c1, c2);
    }

    #[test]
    fn clock_ds_compares_priority1_before_quality() {
        let a = TestClockDS::default_high_grade()
            .with_priority1(Priority1::new(10))
            .dataset();
        let b = TestClockDS::atomic_grandmaster()
            .with_priority1(Priority1::new(20))
            .dataset();

        // Atomic grandmaster looses againt default high grade because of lower priority1.
        assert_eq!(a.compare(&b), ClockDSComparisonResult::Better);
        assert_eq!(b.compare(&a), ClockDSComparisonResult::Worse);
    }

    #[test]
    fn clock_ds_compares_priority2_after_quality() {
        let a = TestClockDS::default_high_grade()
            .with_clock_identity(ClockIdentity::new(&[0x00; 8]))
            .with_priority2(Priority2::new(10))
            .dataset();
        let b = TestClockDS::default_high_grade()
            .with_clock_identity(ClockIdentity::new(&[0x01; 8]))
            .with_priority2(Priority2::new(20))
            .dataset();

        // Same p1 and quality; lower p2 wins.
        assert_eq!(a.compare(&b), ClockDSComparisonResult::Better);
        assert_eq!(b.compare(&a), ClockDSComparisonResult::Worse);
    }

    #[test]
    fn clock_ds_tiebreaks_on_identity_last() {
        let a = TestClockDS::default_high_grade()
            .with_clock_identity(ClockIdentity::new(&[0x00; 8]))
            .dataset();
        let b = TestClockDS::default_high_grade()
            .with_clock_identity(ClockIdentity::new(&[0x01; 8]))
            .dataset();

        // All fields equal except identity; lower identity better than higher.
        assert_eq!(a.compare(&b), ClockDSComparisonResult::Better);
        assert_eq!(b.compare(&a), ClockDSComparisonResult::Worse);
    }

    #[test]
    fn clock_ds_prefers_lower_steps_removed_when_identity_equal() {
        let a = TestClockDS::default_high_grade()
            .with_steps_removed(StepsRemoved::new(1))
            .dataset();
        let b = TestClockDS::default_high_grade()
            .with_steps_removed(StepsRemoved::new(3))
            .dataset();

        assert_eq!(a.compare(&b), ClockDSComparisonResult::Better);
        assert_eq!(b.compare(&a), ClockDSComparisonResult::Worse);
    }

    #[test]
    fn steps_removed_compare_tiebreaks_on_equal_and_one_apart() {
        assert!(matches!(
            StepsRemoved::new(0).compare(&StepsRemoved::new(0)),
            ClockDSComparisonResult::TopologyTieBreak(_)
        ));

        assert!(matches!(
            StepsRemoved::new(0).compare(&StepsRemoved::new(1)),
            ClockDSComparisonResult::TopologyTieBreak(_)
        ));
        assert!(matches!(
            StepsRemoved::new(1).compare(&StepsRemoved::new(0)),
            ClockDSComparisonResult::TopologyTieBreak(_)
        ));
    }

    #[test]
    fn steps_removed_compare_prefers_lower_when_two_or_more_apart() {
        assert_eq!(
            StepsRemoved::new(0).compare(&StepsRemoved::new(2)),
            ClockDSComparisonResult::Better
        );
        assert_eq!(
            StepsRemoved::new(2).compare(&StepsRemoved::new(0)),
            ClockDSComparisonResult::Worse
        );
    }

    #[test]
    fn steps_removed_compare_uses_saturating_add() {
        assert!(matches!(
            StepsRemoved::new(u16::MAX - 1).compare(&StepsRemoved::new(u16::MAX)),
            ClockDSComparisonResult::TopologyTieBreak(_)
        ));
        assert_eq!(
            StepsRemoved::new(u16::MAX).compare(&StepsRemoved::new(0)),
            ClockDSComparisonResult::Worse
        );
    }

    #[test]
    fn topology_compare_a_less_b_receiver_b_lt_sender_b_is_better() {
        let sender_a = pid(1, 1);
        let receiver_a = pid(1, 2);
        let sender_b = pid(2, 2);
        let receiver_b = pid(2, 1); // receiver_b < sender_b

        let topo_a = Topology::foreign(&sender_a, &receiver_a);
        let topo_b = Topology::foreign(&sender_b, &receiver_b);

        assert_eq!(
            topo_a.compare(Ordering::Less, &topo_b),
            TopologyOrdering::Better
        );
    }

    #[test]
    fn topology_compare_a_less_b_receiver_b_gt_sender_b_is_better_by_topology() {
        let sender_a = pid(1, 1);
        let receiver_a = pid(1, 2);
        let sender_b = pid(2, 1);
        let receiver_b = pid(2, 2); // receiver_b > sender_b

        let topo_a = Topology::foreign(&sender_a, &receiver_a);
        let topo_b = Topology::foreign(&sender_b, &receiver_b);

        assert_eq!(
            topo_a.compare(Ordering::Less, &topo_b),
            TopologyOrdering::BetterByTopology
        );
    }

    #[test]
    fn topology_compare_a_less_b_receiver_b_eq_sender_b_is_error1() {
        let sender_a = pid(1, 1);
        let receiver_a = pid(1, 2);
        let sender_b = pid(2, 1);
        let receiver_b = sender_b; // receiver_b == sender_b

        let topo_a = Topology::foreign(&sender_a, &receiver_a);
        let topo_b = Topology::foreign(&sender_b, &receiver_b);

        assert_eq!(
            topo_a.compare(Ordering::Less, &topo_b),
            TopologyOrdering::Error1
        );
    }

    #[test]
    fn topology_compare_a_greater_b_receiver_a_lt_sender_a_is_worse() {
        let sender_a = pid(2, 2);
        let receiver_a = pid(2, 1); // receiver_a < sender_a
        let sender_b = pid(1, 1);
        let receiver_b = pid(1, 2);

        let topo_a = Topology::foreign(&sender_a, &receiver_a);
        let topo_b = Topology::foreign(&sender_b, &receiver_b);

        assert_eq!(
            topo_a.compare(Ordering::Greater, &topo_b),
            TopologyOrdering::Worse
        );
    }

    #[test]
    fn topology_compare_a_greater_b_receiver_a_gt_sender_a_is_worse_by_topology() {
        let sender_a = pid(2, 1);
        let receiver_a = pid(2, 2); // receiver_a > sender_a
        let sender_b = pid(1, 1);
        let receiver_b = pid(1, 2);

        let topo_a = Topology::foreign(&sender_a, &receiver_a);
        let topo_b = Topology::foreign(&sender_b, &receiver_b);

        assert_eq!(
            topo_a.compare(Ordering::Greater, &topo_b),
            TopologyOrdering::WorseByTopology
        );
    }

    #[test]
    fn topology_compare_a_greater_b_receiver_a_eq_sender_a_is_error1() {
        let sender_a = pid(2, 1);
        let receiver_a = sender_a; // receiver_a == sender_a
        let sender_b = pid(1, 1);
        let receiver_b = pid(1, 2);

        let topo_a = Topology::foreign(&sender_a, &receiver_a);
        let topo_b = Topology::foreign(&sender_b, &receiver_b);

        assert_eq!(
            topo_a.compare(Ordering::Greater, &topo_b),
            TopologyOrdering::Error1
        );
    }

    #[test]
    fn topology_compare_a_eq_b_sender_a_lt_sender_b_is_better_by_topology() {
        let sender_a = pid(1, 1);
        let receiver_a = pid(9, 10);
        let sender_b = pid(2, 1);
        let receiver_b = pid(9, 11);

        let topo_a = Topology::foreign(&sender_a, &receiver_a);
        let topo_b = Topology::foreign(&sender_b, &receiver_b);

        assert_eq!(
            topo_a.compare(Ordering::Equal, &topo_b),
            TopologyOrdering::BetterByTopology
        );
    }

    #[test]
    fn topology_compare_a_eq_b_sender_a_gt_sender_b_is_worse_by_topology() {
        let sender_a = pid(2, 1);
        let receiver_a = pid(9, 10);
        let sender_b = pid(1, 1);
        let receiver_b = pid(9, 11);

        let topo_a = Topology::foreign(&sender_a, &receiver_a);
        let topo_b = Topology::foreign(&sender_b, &receiver_b);

        assert_eq!(
            topo_a.compare(Ordering::Equal, &topo_b),
            TopologyOrdering::WorseByTopology
        );
    }

    #[test]
    fn topology_compare_a_eq_b_sender_equal_receiver_port_number_breaks_tie() {
        let sender_a = pid(1, 1);
        let sender_b = sender_a;
        let receiver_a = pid(9, 10);
        let receiver_b = pid(9, 11);

        let topo_a = Topology::foreign(&sender_a, &receiver_a);
        let topo_b = Topology::foreign(&sender_b, &receiver_b);

        assert_eq!(
            topo_a.compare(Ordering::Equal, &topo_b),
            TopologyOrdering::BetterByTopology
        );
        assert_eq!(
            topo_b.compare(Ordering::Equal, &topo_a),
            TopologyOrdering::WorseByTopology
        );
    }

    #[test]
    fn topology_compare_a_eq_b_sender_equal_receiver_port_number_equal_is_error2() {
        let sender_a = pid(1, 1);
        let sender_b = sender_a;
        let receiver_a = pid(9, 10);
        let receiver_b = pid(7, 10); // same port number => error-2, independent of clock identity

        let topo_a = Topology::foreign(&sender_a, &receiver_a);
        let topo_b = Topology::foreign(&sender_b, &receiver_b);

        assert_eq!(
            topo_a.compare(Ordering::Equal, &topo_b),
            TopologyOrdering::Error2
        );
    }

    #[test]
    fn topology_compare_is_antisymmetric_for_non_error_results() {
        fn reverse(ordering: Ordering) -> Ordering {
            match ordering {
                Ordering::Less => Ordering::Greater,
                Ordering::Greater => Ordering::Less,
                Ordering::Equal => Ordering::Equal,
            }
        }

        fn assert_antisymmetric(a: &Topology<'_>, ab: Ordering, b: &Topology<'_>) {
            let res_ab = a.compare(ab, b);
            let res_ba = b.compare(reverse(ab), a);

            match res_ab {
                TopologyOrdering::Better => assert_eq!(res_ba, TopologyOrdering::Worse),
                TopologyOrdering::BetterByTopology => {
                    assert_eq!(res_ba, TopologyOrdering::WorseByTopology)
                }
                TopologyOrdering::Worse => assert_eq!(res_ba, TopologyOrdering::Better),
                TopologyOrdering::WorseByTopology => {
                    assert_eq!(res_ba, TopologyOrdering::BetterByTopology)
                }
                TopologyOrdering::Error1 | TopologyOrdering::Error2 => {}
            }
        }

        // A < B: receiver_b < sender_b => Better/Worse
        let a_sender = pid(1, 1);
        let a_receiver = pid(1, 2);
        let b_sender = pid(2, 2);
        let b_receiver = pid(2, 1);
        let a = Topology::foreign(&a_sender, &a_receiver);
        let b = Topology::foreign(&b_sender, &b_receiver);
        assert_antisymmetric(&a, Ordering::Less, &b);

        // A < B: receiver_b > sender_b => BetterByTopology/WorseByTopology
        let a_sender = pid(1, 1);
        let a_receiver = pid(1, 2);
        let b_sender = pid(2, 1);
        let b_receiver = pid(2, 2);
        let a = Topology::foreign(&a_sender, &a_receiver);
        let b = Topology::foreign(&b_sender, &b_receiver);
        assert_antisymmetric(&a, Ordering::Less, &b);

        // A == B: sender order tie-break
        let a_sender = pid(1, 1);
        let a_receiver = pid(9, 10);
        let b_sender = pid(2, 1);
        let b_receiver = pid(9, 11);
        let a = Topology::foreign(&a_sender, &a_receiver);
        let b = Topology::foreign(&b_sender, &b_receiver);
        assert_antisymmetric(&a, Ordering::Equal, &b);
    }
}
