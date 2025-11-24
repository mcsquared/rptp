use std::ops::Range;
use std::time::Duration;

use crate::clock::{ClockIdentity, ClockQuality, LocalClock, StepsRemoved, SynchronizableClock};
use crate::log::PortLog;
use crate::message::{AnnounceMessage, SequenceId};
use crate::port::{LogInterval, ParentPortIdentity, Port, PortIdentity, PortTimingPolicy};
use crate::portstate::PortState;

pub trait SortedForeignClockRecords {
    fn insert(&mut self, record: ForeignClockRecord);
    fn update_record<F>(&mut self, source_port_identity: &PortIdentity, update: F) -> bool
    where
        F: FnOnce(&mut ForeignClockRecord);
    fn first(&self) -> Option<&ForeignClockRecord>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Priority1(u8);

impl Priority1 {
    pub const fn new(value: u8) -> Self {
        Self(value)
    }

    pub fn as_u8(self) -> u8 {
        self.0
    }
}

impl From<u8> for Priority1 {
    fn from(value: u8) -> Self {
        Self::new(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct BmcaRank<'a> {
    identity: &'a ClockIdentity,
    priority1: &'a Priority1,
    quality: &'a ClockQuality,
    priority2: &'a Priority2,
    steps_removed: &'a StepsRemoved,
}

impl BmcaRank<'_> {
    pub fn better_than(&self, other: &BmcaRank) -> bool {
        if self.identity == other.identity {
            return self.steps_removed < other.steps_removed;
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

        a < b
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Priority2(u8);

impl Priority2 {
    pub const fn new(value: u8) -> Self {
        Self(value)
    }

    pub fn as_u8(self) -> u8 {
        self.0
    }
}

impl From<u8> for Priority2 {
    fn from(value: u8) -> Self {
        Self::new(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForeignClockDS {
    identity: ClockIdentity,
    priority1: Priority1,
    priority2: Priority2,
    quality: ClockQuality,
    steps_removed: StepsRemoved,
}

impl ForeignClockDS {
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

    pub fn is_grandmaster_capable(&self) -> bool {
        self.quality.is_grandmaster_capable()
    }

    pub fn steps_removed(&self) -> StepsRemoved {
        self.steps_removed
    }

    pub fn from_slice(buf: &[u8; 16]) -> Self {
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
            priority1: Priority1::new(buf[ForeignClockDS::PRIORITY1_OFFSET]),
            priority2: Priority2::new(buf[ForeignClockDS::PRIORITY2_OFFSET]),
            quality: ClockQuality::from_slice(&[
                buf[ForeignClockDS::QUALITY_RANGE.start],
                buf[ForeignClockDS::QUALITY_RANGE.start + 1],
                buf[ForeignClockDS::QUALITY_RANGE.start + 2],
                buf[ForeignClockDS::QUALITY_RANGE.start + 3],
            ]),
            steps_removed: StepsRemoved::new(u16::from_be_bytes([
                buf[ForeignClockDS::STEPS_REMOVED_OFFSET.start],
                buf[ForeignClockDS::STEPS_REMOVED_OFFSET.start + 1],
            ])),
        }
    }

    pub fn better_than(&self, other: &ForeignClockDS) -> bool {
        let a = BmcaRank {
            identity: &self.identity,
            priority1: &self.priority1,
            quality: &self.quality,
            priority2: &self.priority2,
            steps_removed: &self.steps_removed,
        };
        let b = BmcaRank {
            identity: &other.identity,
            priority1: &other.priority1,
            quality: &other.quality,
            priority2: &other.priority2,
            steps_removed: &other.steps_removed,
        };

        a.better_than(&b)
    }

    fn worse_than(&self, other: &BmcaRank) -> bool {
        let own_rank = BmcaRank {
            identity: &self.identity,
            priority1: &self.priority1,
            quality: &self.quality,
            priority2: &self.priority2,
            steps_removed: &self.steps_removed,
        };

        other.better_than(&own_rank)
    }

    pub fn to_bytes(&self) -> [u8; 16] {
        let mut bytes = [0u8; 16];
        bytes[Self::PRIORITY1_OFFSET] = self.priority1.as_u8();
        bytes[Self::QUALITY_RANGE].copy_from_slice(&self.quality.to_bytes());
        bytes[Self::PRIORITY2_OFFSET] = self.priority2.as_u8();
        bytes[Self::IDENTITY_RANGE].copy_from_slice(self.identity.as_bytes());
        bytes[Self::STEPS_REMOVED_OFFSET.start..Self::STEPS_REMOVED_OFFSET.end]
            .copy_from_slice(&self.steps_removed.to_be_bytes());
        bytes
    }
}

pub struct DefaultDS {
    identity: ClockIdentity,
    priority1: Priority1,
    priority2: Priority2,
    quality: ClockQuality,
}

impl DefaultDS {
    pub fn new(
        identity: ClockIdentity,
        priority1: Priority1,
        priority2: Priority2,
        quality: ClockQuality,
    ) -> Self {
        Self {
            identity,
            priority1,
            priority2,
            quality,
        }
    }

    pub fn identity(&self) -> &ClockIdentity {
        &self.identity
    }

    pub fn is_grandmaster_capable(&self) -> bool {
        self.quality.is_grandmaster_capable()
    }

    pub fn better_than(&self, foreign: &ForeignClockDS, steps_removed: &StepsRemoved) -> bool {
        foreign.worse_than(&BmcaRank {
            identity: &self.identity,
            priority1: &self.priority1,
            priority2: &self.priority2,
            quality: &self.quality,
            steps_removed,
        })
    }

    pub fn announce(
        &self,
        sequence_id: SequenceId,
        steps_removed: StepsRemoved,
    ) -> AnnounceMessage {
        AnnounceMessage::new(
            sequence_id,
            ForeignClockDS::new(
                self.identity,
                self.priority1,
                self.priority2,
                self.quality,
                steps_removed,
            ),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForeignClockRecord {
    source_port_identity: PortIdentity,
    last_announce: AnnounceMessage,
    foreign_clock: Option<ForeignClockDS>,
}

impl ForeignClockRecord {
    pub fn new(source_port_identity: PortIdentity, announce: AnnounceMessage) -> Self {
        Self {
            source_port_identity,
            last_announce: announce,
            foreign_clock: None,
        }
    }

    pub fn same_source_as(&self, source_port_identity: &PortIdentity) -> bool {
        self.source_port_identity == *source_port_identity
    }

    pub fn consider(&mut self, announce: AnnounceMessage) -> Option<&ForeignClockDS> {
        if let Some(clock) = announce.follows(self.last_announce) {
            self.foreign_clock = Some(clock);
        } else {
            self.foreign_clock = None;
        }

        self.last_announce = announce;
        self.foreign_clock.as_ref()
    }

    pub fn clock(&self) -> Option<&ForeignClockDS> {
        self.foreign_clock.as_ref()
    }

    pub fn source_port_identity(&self) -> PortIdentity {
        self.source_port_identity
    }
}

impl Ord for ForeignClockRecord {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering::*;

        let ord = match (self.clock(), other.clock()) {
            (Some(&clock1), Some(&clock2)) => {
                if clock1.better_than(&clock2) {
                    Less
                } else if clock2.better_than(&clock1) {
                    Greater
                } else {
                    Equal
                }
            }
            (Some(_), None) => Less,
            (None, Some(_)) => Greater,
            (None, None) => Equal,
        };

        if ord == Equal {
            self.source_port_identity.cmp(&other.source_port_identity)
        } else {
            ord
        }
    }
}

impl PartialOrd for ForeignClockRecord {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

// Master decision point as defined in IEEE 1588-2019 Section 9.3.1 and Figure 26
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BmcaMasterDecisionPoint {
    M1,
    M2,
    M3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BmcaDecision {
    Undecided,
    Master(BmcaMasterDecision),
    Slave(BmcaSlaveDecision),
    Passive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BmcaMasterDecision {
    decision_point: BmcaMasterDecisionPoint,
    steps_removed: StepsRemoved,
}

impl BmcaMasterDecision {
    pub fn new(decision_point: BmcaMasterDecisionPoint, steps_removed: StepsRemoved) -> Self {
        Self {
            decision_point,
            steps_removed,
        }
    }

    pub fn apply<P: Port, B: Bmca, L: PortLog>(
        &self,
        port: P,
        bmca: B,
        log: L,
        timing_policy: PortTimingPolicy,
    ) -> PortState<P, B, L> {
        let qualification_timeout_policy =
            QualificationTimeoutPolicy::new(self.decision_point, self.steps_removed);

        PortState::pre_master(port, bmca, log, timing_policy, qualification_timeout_policy)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BmcaSlaveDecision {
    parent_port_identity: ParentPortIdentity,
    steps_removed: StepsRemoved,
}

impl BmcaSlaveDecision {
    pub fn new(parent_port_identity: ParentPortIdentity, steps_removed: StepsRemoved) -> Self {
        Self {
            parent_port_identity,
            steps_removed,
        }
    }

    pub fn parent_port_identity(&self) -> &ParentPortIdentity {
        &self.parent_port_identity
    }

    pub fn apply<P: Port, B: Bmca, L: PortLog>(
        self,
        port: P,
        bmca: B,
        log: L,
        timing_policy: PortTimingPolicy,
    ) -> PortState<P, B, L> {
        PortState::uncalibrated(port, bmca, log, self.parent_port_identity, timing_policy)
    }
}

pub trait Bmca {
    fn consider(&mut self, source_port_identity: PortIdentity, announce: AnnounceMessage);
    fn decision<C: SynchronizableClock>(&self, local_clock: &LocalClock<C>) -> BmcaDecision;
}

pub struct FullBmca<S: SortedForeignClockRecords> {
    sorted_clock_records: S,
}

impl<S: SortedForeignClockRecords> FullBmca<S> {
    pub fn new(sorted_clock_records: S) -> Self {
        Self {
            sorted_clock_records,
        }
    }
}

impl<S: SortedForeignClockRecords> Bmca for FullBmca<S> {
    fn consider(&mut self, source_port_identity: PortIdentity, announce: AnnounceMessage) {
        let updated = self
            .sorted_clock_records
            .update_record(&source_port_identity, |record| {
                record.consider(announce);
            });

        if !updated {
            self.sorted_clock_records
                .insert(ForeignClockRecord::new(source_port_identity, announce));
        }
    }

    // IEEE 1588-2019 Section 9.3.3 - BMCA State Decision Algorithm
    // Note: variable names correspond to those in the spec for easier reference.
    fn decision<C: SynchronizableClock>(&self, local_clock: &LocalClock<C>) -> BmcaDecision {
        let d_0 = local_clock;

        let Some(e_rbest_record) = self.sorted_clock_records.first() else {
            return BmcaDecision::Undecided;
        };

        let Some(e_rbest) = e_rbest_record.clock() else {
            return BmcaDecision::Undecided;
        };

        if d_0.is_grandmaster_capable() {
            d0_better_or_better_by_topology_than_e_rbest(d_0, e_rbest)
        } else {
            d0_better_or_better_by_topology_than_e_best(
                d_0,
                e_rbest, // TODO: once multi-port is supported, introduce e_best here
                e_rbest,
                e_rbest_record.source_port_identity(),
            )
        }
    }
}

fn d0_better_or_better_by_topology_than_e_rbest<C: SynchronizableClock>(
    d_0: &LocalClock<C>,
    e_rbest: &ForeignClockDS,
) -> BmcaDecision {
    if d_0.better_than(e_rbest) {
        BmcaDecision::Master(BmcaMasterDecision::new(
            BmcaMasterDecisionPoint::M1,
            StepsRemoved::new(0), // steps removed to zero as per IEEE 1588-2019 Section 9.3.5, Table 13
        ))
    } else {
        BmcaDecision::Passive // Passive decision point P1
    }
}

fn d0_better_or_better_by_topology_than_e_best<C: SynchronizableClock>(
    d_0: &LocalClock<C>,
    e_best: &ForeignClockDS,
    _e_rbest: &ForeignClockDS,
    source_port_identity: PortIdentity,
) -> BmcaDecision {
    if d_0.better_than(e_best) {
        BmcaDecision::Master(BmcaMasterDecision::new(
            BmcaMasterDecisionPoint::M2,
            StepsRemoved::new(0), // steps removed to zero as per IEEE 1588-2019 Section 9.3.5, Table 13
        ))
    } else {
        // TODO: as the implementation supports only a single port at the
        // moment, we can directly recommend slave here, which corresponds to
        // slave decision point S1. In a multi-port implementation, we would
        // need to compare against e_best. When e_rbest == e_best, we'd had
        // slave decision point S1, we'd compare e_best against e_rbest and
        // decide between master decision point M3 and passive decision point P2.
        BmcaDecision::Slave(BmcaSlaveDecision::new(
            ParentPortIdentity::new(source_port_identity),
            e_best.steps_removed().increment(),
        ))
    }
}

pub struct NoopBmca;

impl Bmca for NoopBmca {
    fn consider(
        &mut self,
        _source_port_identity: crate::port::PortIdentity,
        _announce: crate::message::AnnounceMessage,
    ) {
    }
    fn decision<C: crate::clock::SynchronizableClock>(
        &self,
        _local_clock: &LocalClock<C>,
    ) -> BmcaDecision {
        BmcaDecision::Undecided
    }
}

// Qualification timeout policy as defined in IEEE 1588-2019 Section 9.2.6.10
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QualificationTimeoutPolicy {
    master_decision_point: BmcaMasterDecisionPoint,
    steps_removed: StepsRemoved,
}

impl QualificationTimeoutPolicy {
    pub fn new(
        master_decision_point: BmcaMasterDecisionPoint,
        steps_removed: StepsRemoved,
    ) -> Self {
        Self {
            master_decision_point,
            steps_removed,
        }
    }

    pub fn duration(&self, log_announce_interval: LogInterval) -> Duration {
        match self.master_decision_point {
            BmcaMasterDecisionPoint::M1 => Duration::from_secs(0),
            BmcaMasterDecisionPoint::M2 => Duration::from_secs(0),
            BmcaMasterDecisionPoint::M3 => {
                let n = self.steps_removed.as_u16() as u32 + 1;
                log_announce_interval.duration() * n
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    use crate::clock::FakeClock;
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::port::PortNumber;

    const CLK_ID_HIGH: ClockIdentity =
        ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x01]);
    const CLK_ID_MID: ClockIdentity =
        ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x02]);
    const CLK_ID_LOW: ClockIdentity =
        ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x03]);
    const CLK_ID_GM: ClockIdentity =
        ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x04]);

    const CLK_QUALITY_HIGH: ClockQuality = ClockQuality::new(248, 0xFE, 0xFFFF);
    const CLK_QUALITY_MID: ClockQuality = ClockQuality::new(250, 0xFE, 0xFFFF);
    const CLK_QUALITY_LOW: ClockQuality = ClockQuality::new(255, 0xFF, 0xFFFF);
    const CLK_QUALITY_GM: ClockQuality = ClockQuality::new(100, 0xFE, 0xFFFF);

    impl ForeignClockDS {
        pub(crate) fn high_grade_test_clock() -> ForeignClockDS {
            ForeignClockDS::new(
                CLK_ID_HIGH,
                Priority1::new(127),
                Priority2::new(127),
                CLK_QUALITY_HIGH,
                StepsRemoved::new(0),
            )
        }

        pub(crate) fn mid_grade_test_clock() -> ForeignClockDS {
            ForeignClockDS::new(
                CLK_ID_MID,
                Priority1::new(127),
                Priority2::new(127),
                CLK_QUALITY_MID,
                StepsRemoved::new(0),
            )
        }

        pub(crate) fn low_grade_test_clock() -> ForeignClockDS {
            ForeignClockDS::new(
                CLK_ID_LOW,
                Priority1::new(127),
                Priority2::new(127),
                CLK_QUALITY_LOW,
                StepsRemoved::new(0),
            )
        }
    }

    impl DefaultDS {
        pub(crate) fn high_grade_test_clock() -> DefaultDS {
            DefaultDS::new(
                CLK_ID_HIGH,
                Priority1::new(127),
                Priority2::new(127),
                CLK_QUALITY_HIGH,
            )
        }

        pub(crate) fn mid_grade_test_clock() -> DefaultDS {
            DefaultDS::new(
                CLK_ID_MID,
                Priority1::new(127),
                Priority2::new(127),
                CLK_QUALITY_MID,
            )
        }

        pub(crate) fn low_grade_test_clock() -> DefaultDS {
            DefaultDS::new(
                CLK_ID_LOW,
                Priority1::new(127),
                Priority2::new(127),
                CLK_QUALITY_LOW,
            )
        }

        pub(crate) fn gm_grade_test_clock() -> DefaultDS {
            DefaultDS::new(
                CLK_ID_GM,
                Priority1::new(127),
                Priority2::new(127),
                CLK_QUALITY_GM,
            )
        }
    }

    impl ForeignClockRecord {
        pub(crate) fn with_resolved_clock(self, foreign_clock: ForeignClockDS) -> Self {
            Self {
                source_port_identity: self.source_port_identity,
                last_announce: self.last_announce,
                foreign_clock: Some(foreign_clock),
            }
        }
    }

    #[test]
    fn test_foreign_clock_ordering() {
        let high = ForeignClockDS::high_grade_test_clock();
        let mid = ForeignClockDS::mid_grade_test_clock();
        let low = ForeignClockDS::low_grade_test_clock();

        assert!(high.better_than(&mid));
        assert!(!mid.better_than(&high));

        assert!(high.better_than(&low));
        assert!(!low.better_than(&high));

        assert!(mid.better_than(&low));
        assert!(!low.better_than(&mid));
    }

    #[test]
    fn test_foreign_clock_equality() {
        let c1 = ForeignClockDS::high_grade_test_clock();
        let c2 = ForeignClockDS::high_grade_test_clock();

        assert!(!c1.better_than(&c2));
        assert!(!c2.better_than(&c1));
        assert_eq!(c1, c2);
    }

    #[test]
    fn foreign_clock_ds_compares_priority1_before_quality() {
        // a has lower priority1 but worse quality; still better than b.
        let a = ForeignClockDS::new(
            CLK_ID_LOW,
            Priority1::new(10),
            Priority2::new(127),
            CLK_QUALITY_LOW,
            StepsRemoved::new(0),
        );
        let b = ForeignClockDS::new(
            CLK_ID_HIGH,
            Priority1::new(100),
            Priority2::new(127),
            CLK_QUALITY_HIGH,
            StepsRemoved::new(0),
        );

        assert!(a.better_than(&b));
        assert!(!b.better_than(&a));
    }

    #[test]
    fn foreign_clock_ds_compares_priority2_after_quality() {
        // Same p1 and quality; lower p2 wins.
        let a = ForeignClockDS::new(
            CLK_ID_LOW,
            Priority1::new(127),
            Priority2::new(10),
            CLK_QUALITY_HIGH,
            StepsRemoved::new(0),
        );
        let b = ForeignClockDS::new(
            CLK_ID_HIGH,
            Priority1::new(127),
            Priority2::new(20),
            CLK_QUALITY_HIGH,
            StepsRemoved::new(0),
        );

        assert!(a.better_than(&b));
        assert!(!b.better_than(&a));
    }

    #[test]
    fn foreign_clock_ds_tiebreaks_on_identity_last() {
        // All fields equal except identity; lower identity better than higher.
        let a = ForeignClockDS::new(
            CLK_ID_HIGH,
            Priority1::new(127),
            Priority2::new(127),
            CLK_QUALITY_HIGH,
            StepsRemoved::new(0),
        );
        let b = ForeignClockDS::new(
            CLK_ID_MID,
            Priority1::new(127),
            Priority2::new(127),
            CLK_QUALITY_HIGH,
            StepsRemoved::new(0),
        );

        assert!(a.better_than(&b));
        assert!(!b.better_than(&a));
    }

    #[test]
    fn foreign_clock_ds_prefers_lower_steps_removed_when_identity_equal() {
        let a = ForeignClockDS::new(
            CLK_ID_HIGH,
            Priority1::new(127),
            Priority2::new(127),
            CLK_QUALITY_HIGH,
            StepsRemoved::new(1),
        );
        let b = ForeignClockDS::new(
            CLK_ID_HIGH,
            Priority1::new(127),
            Priority2::new(127),
            CLK_QUALITY_HIGH,
            StepsRemoved::new(3),
        );

        assert!(a.better_than(&b));
        assert!(!b.better_than(&a));
    }

    #[test]
    fn full_bmca_gm_capable_local_with_no_qualified_foreign_is_undecided() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::gm_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());

        assert_eq!(bmca.decision(&local_clock), BmcaDecision::Undecided);
    }

    #[test]
    fn full_bmca_gm_capable_local_loses_tuple_returns_passive() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::gm_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let mut bmca = FullBmca::new(SortedForeignClockRecordsVec::new());

        // Foreign uses lower priority1 so it is better, even though clock class is worse.
        let foreign_strong = ForeignClockDS::new(
            CLK_ID_HIGH,
            Priority1::new(1),
            Priority2::new(127),
            CLK_QUALITY_HIGH,
            StepsRemoved::new(0),
        );
        let port_id = PortIdentity::new(CLK_ID_HIGH, PortNumber::new(1));

        bmca.consider(port_id, AnnounceMessage::new(0.into(), foreign_strong));
        bmca.consider(port_id, AnnounceMessage::new(1.into(), foreign_strong));

        assert_eq!(bmca.decision(&local_clock), BmcaDecision::Passive);
    }

    #[test]
    fn full_bmca_gm_capable_local_better_than_foreign_returns_master_m1() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::gm_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let mut bmca = FullBmca::new(SortedForeignClockRecordsVec::new());

        // Foreign is worse (higher priority1), so local GM-capable should become Master(M1).
        let foreign = ForeignClockDS::mid_grade_test_clock();
        let port_id = PortIdentity::new(CLK_ID_MID, PortNumber::new(1));

        bmca.consider(port_id, AnnounceMessage::new(0.into(), foreign));
        bmca.consider(port_id, AnnounceMessage::new(1.into(), foreign));

        assert_eq!(
            bmca.decision(&local_clock),
            BmcaDecision::Master(BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M1,
                StepsRemoved::new(0)
            ))
        );
    }

    #[test]
    fn full_bmca_non_gm_local_better_than_foreign_returns_master_m2() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let mut bmca = FullBmca::new(SortedForeignClockRecordsVec::new());

        // Foreign is slightly worse quality; local should be better and take M2.
        let foreign = ForeignClockDS::low_grade_test_clock();
        let port_id = PortIdentity::new(CLK_ID_LOW, PortNumber::new(1));

        bmca.consider(port_id, AnnounceMessage::new(0.into(), foreign));
        bmca.consider(port_id, AnnounceMessage::new(1.into(), foreign));

        assert_eq!(
            bmca.decision(&local_clock),
            BmcaDecision::Master(BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M2,
                StepsRemoved::new(0)
            ))
        );
    }

    #[test]
    fn full_bmca_non_gm_local_loses_tuple_returns_slave() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let mut bmca = FullBmca::new(SortedForeignClockRecordsVec::new());

        let foreign = ForeignClockDS::high_grade_test_clock();
        let port_id = PortIdentity::new(CLK_ID_HIGH, PortNumber::new(1));

        bmca.consider(port_id, AnnounceMessage::new(0.into(), foreign));
        bmca.consider(port_id, AnnounceMessage::new(1.into(), foreign));

        assert_eq!(
            bmca.decision(&local_clock),
            BmcaDecision::Slave(BmcaSlaveDecision::new(
                ParentPortIdentity::new(port_id),
                foreign.steps_removed().increment()
            )),
        );
    }

    #[test]
    fn full_bmca_recommends_slave_from_interleaved_announce_sequence() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::low_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let mut bmca = FullBmca::new(SortedForeignClockRecordsVec::new());

        let foreign_high = ForeignClockDS::high_grade_test_clock();
        let foreign_mid = ForeignClockDS::mid_grade_test_clock();

        let port_id_high = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
            PortNumber::new(1),
        );
        let port_id_mid = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x02]),
            PortNumber::new(1),
        );

        bmca.consider(port_id_high, AnnounceMessage::new(0.into(), foreign_high));
        bmca.consider(port_id_mid, AnnounceMessage::new(0.into(), foreign_mid));
        bmca.consider(port_id_high, AnnounceMessage::new(1.into(), foreign_high));
        bmca.consider(port_id_mid, AnnounceMessage::new(1.into(), foreign_mid));

        let decision = bmca.decision(&local_clock);

        assert_eq!(
            decision,
            BmcaDecision::Slave(BmcaSlaveDecision::new(
                ParentPortIdentity::new(port_id_high),
                foreign_high.steps_removed().increment()
            )),
        );
    }

    #[test]
    fn full_bmca_recommends_slave_from_non_interleaved_announce_sequence() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::low_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let mut bmca = FullBmca::new(SortedForeignClockRecordsVec::new());

        let foreign_high = ForeignClockDS::high_grade_test_clock();
        let foreign_mid = ForeignClockDS::mid_grade_test_clock();

        let port_id_high = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
            PortNumber::new(1),
        );
        let port_id_mid = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x02]),
            PortNumber::new(1),
        );

        bmca.consider(port_id_high, AnnounceMessage::new(0.into(), foreign_high));
        bmca.consider(port_id_high, AnnounceMessage::new(1.into(), foreign_high));
        bmca.consider(port_id_mid, AnnounceMessage::new(0.into(), foreign_mid));
        bmca.consider(port_id_mid, AnnounceMessage::new(1.into(), foreign_mid));

        let decision = bmca.decision(&local_clock);

        assert_eq!(
            decision,
            BmcaDecision::Slave(BmcaSlaveDecision::new(
                ParentPortIdentity::new(port_id_high),
                foreign_high.steps_removed().increment()
            ))
        );
    }

    #[test]
    fn full_bmca_undecided_when_no_announces_yet() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());

        assert_eq!(bmca.decision(&local_clock), BmcaDecision::Undecided);
    }

    #[test]
    fn full_bmca_undecided_when_no_qualified_clock_records_yet() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let mut bmca = FullBmca::new(SortedForeignClockRecordsVec::new());

        let foreign_high = ForeignClockDS::high_grade_test_clock();

        bmca.consider(
            PortIdentity::fake(),
            AnnounceMessage::new(0.into(), foreign_high),
        );

        assert_eq!(bmca.decision(&local_clock), BmcaDecision::Undecided);
    }

    #[test]
    fn full_bmca_undecided_when_only_single_announces_each() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let mut bmca = FullBmca::new(SortedForeignClockRecordsVec::new());

        let foreign_high = ForeignClockDS::high_grade_test_clock();
        let foreign_mid = ForeignClockDS::mid_grade_test_clock();
        let foreign_low = ForeignClockDS::low_grade_test_clock();

        bmca.consider(
            PortIdentity::fake(),
            AnnounceMessage::new(0.into(), foreign_high),
        );
        bmca.consider(
            PortIdentity::fake(),
            AnnounceMessage::new(5.into(), foreign_mid),
        );
        bmca.consider(
            PortIdentity::fake(),
            AnnounceMessage::new(10.into(), foreign_low),
        );

        assert_eq!(bmca.decision(&local_clock), BmcaDecision::Undecided);
    }

    #[test]
    fn full_bmca_undecided_on_sequence_gap() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let mut bmca = FullBmca::new(SortedForeignClockRecordsVec::new());

        let foreign_high = ForeignClockDS::high_grade_test_clock();

        bmca.consider(
            PortIdentity::fake(),
            AnnounceMessage::new(0.into(), foreign_high),
        );
        bmca.consider(
            PortIdentity::fake(),
            AnnounceMessage::new(2.into(), foreign_high),
        );

        assert_eq!(bmca.decision(&local_clock), BmcaDecision::Undecided);
    }

    #[test]
    fn full_bmca_undecided_on_sequence_gap_after_being_qualified_before() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let mut bmca = FullBmca::new(SortedForeignClockRecordsVec::new());
        let foreign_high = ForeignClockDS::high_grade_test_clock();

        bmca.consider(
            PortIdentity::fake(),
            AnnounceMessage::new(0.into(), foreign_high),
        );
        bmca.consider(
            PortIdentity::fake(),
            AnnounceMessage::new(1.into(), foreign_high),
        );

        assert!(matches!(
            bmca.decision(&local_clock),
            BmcaDecision::Slave(_)
        ));

        bmca.consider(
            PortIdentity::fake(),
            AnnounceMessage::new(3.into(), foreign_high),
        );

        assert_eq!(bmca.decision(&local_clock), BmcaDecision::Undecided);
    }

    #[test]
    fn qualification_timeout_policy_duration_m1_is_zero() {
        let policy =
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M1, StepsRemoved::new(5));

        let qualification_timeout_interval = policy.duration(LogInterval::new(4));

        assert_eq!(qualification_timeout_interval, Duration::from_secs(0));
    }

    #[test]
    fn qualification_timeout_policy_duration_m2_is_zero() {
        let policy =
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M2, StepsRemoved::new(5));

        let qualification_timeout_interval = policy.duration(LogInterval::new(4));

        assert_eq!(qualification_timeout_interval, Duration::from_secs(0));
    }

    #[test]
    fn qualification_timeout_policy_duration_m3_scales_with_steps_removed() {
        let policy =
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M3, StepsRemoved::new(0));
        let qualification_timeout_interval = policy.duration(LogInterval::new(0));
        assert_eq!(qualification_timeout_interval, Duration::from_secs(1));

        let policy =
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M3, StepsRemoved::new(1));
        let qualification_timeout_interval = policy.duration(LogInterval::new(0));
        assert_eq!(qualification_timeout_interval, Duration::from_secs(2));

        let policy =
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M3, StepsRemoved::new(2));
        let qualification_timeout_interval = policy.duration(LogInterval::new(1));
        assert_eq!(qualification_timeout_interval, Duration::from_secs(6));

        let policy =
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M3, StepsRemoved::new(3));
        let qualification_timeout_interval = policy.duration(LogInterval::new(2));
        assert_eq!(qualification_timeout_interval, Duration::from_secs(16));
    }

    #[test]
    fn qualification_timeout_policy_duration_m3_large_inputs() {
        let policy = QualificationTimeoutPolicy::new(
            BmcaMasterDecisionPoint::M3,
            StepsRemoved::new(u16::MAX),
        );

        let qualification_timeout_interval = policy.duration(LogInterval::new(10));

        assert_eq!(
            qualification_timeout_interval,
            Duration::from_secs(67_108_864)
        );
    }
}
