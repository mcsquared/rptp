use std::cell::RefCell;
use std::ops::Range;
use std::time::Duration;

use crate::clock::{ClockIdentity, ClockQuality, LocalClock, StepsRemoved, SynchronizableClock};
use crate::message::{AnnounceMessage, SequenceId};
use crate::port::{LogInterval, ParentPortIdentity, PortIdentity};

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
}

impl ForeignClockDS {
    const PRIORITY1_OFFSET: usize = 0;
    const QUALITY_RANGE: Range<usize> = 1..5;
    const PRIORITY2_OFFSET: usize = 5;
    const IDENTITY_RANGE: Range<usize> = 6..14;

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

    pub fn from_slice(buf: &[u8; 14]) -> Self {
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
        }
    }

    pub fn outranks_other(&self, other: &ForeignClockDS) -> bool {
        if self.identity == other.identity {
            return false;
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

    pub fn to_bytes(&self) -> [u8; 14] {
        let mut bytes = [0u8; 14];
        bytes[Self::PRIORITY1_OFFSET] = self.priority1.as_u8();
        bytes[Self::QUALITY_RANGE].copy_from_slice(&self.quality.to_bytes());
        bytes[Self::PRIORITY2_OFFSET] = self.priority2.as_u8();
        bytes[Self::IDENTITY_RANGE].copy_from_slice(self.identity.as_bytes());
        bytes
    }
}

pub struct LocalClockDS {
    ds: ForeignClockDS,
}

impl LocalClockDS {
    pub fn new(
        identity: ClockIdentity,
        priority1: Priority1,
        priority2: Priority2,
        quality: ClockQuality,
    ) -> Self {
        Self {
            ds: ForeignClockDS::new(identity, priority1, priority2, quality),
        }
    }

    pub fn identity(&self) -> &ClockIdentity {
        &self.ds.identity
    }

    pub fn outranks_foreign(&self, foreign: &ForeignClockDS) -> bool {
        self.ds.outranks_other(foreign)
    }

    pub fn announce(&self, sequence_id: SequenceId) -> AnnounceMessage {
        AnnounceMessage::new(sequence_id, self.ds)
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
                if clock1.outranks_other(&clock2) {
                    Less
                } else if clock2.outranks_other(&clock1) {
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
pub enum BmcaRecommendation {
    Undecided,
    Master(QualificationTimeoutPolicy),
    Slave(ParentPortIdentity),
    // TODO: Passive,
}

pub trait Bmca {
    fn consider(&self, source_port_identity: PortIdentity, announce: AnnounceMessage);
    fn recommendation<C: SynchronizableClock>(
        &self,
        local_clock: &LocalClock<C>,
    ) -> BmcaRecommendation;
}

pub struct FullBmca<S: SortedForeignClockRecords> {
    sorted_clock_records: RefCell<S>,
}

impl<S: SortedForeignClockRecords> FullBmca<S> {
    pub fn new(sorted_clock_records: S) -> Self {
        Self {
            sorted_clock_records: RefCell::new(sorted_clock_records),
        }
    }
}

impl<S: SortedForeignClockRecords> Bmca for FullBmca<S> {
    fn consider(&self, source_port_identity: PortIdentity, announce: AnnounceMessage) {
        let updated =
            self.sorted_clock_records
                .borrow_mut()
                .update_record(&source_port_identity, |record| {
                    record.consider(announce);
                });

        if !updated {
            self.sorted_clock_records
                .borrow_mut()
                .insert(ForeignClockRecord::new(source_port_identity, announce));
        }
    }

    fn recommendation<C: SynchronizableClock>(
        &self,
        local_clock: &LocalClock<C>,
    ) -> BmcaRecommendation {
        let recommendation = self
            .sorted_clock_records
            .borrow()
            .first()
            .map(|foreign_record| {
                if let Some(foreign_clock) = foreign_record.clock() {
                    if local_clock.outranks_foreign(foreign_clock) {
                        BmcaRecommendation::Master(QualificationTimeoutPolicy::new(
                            BmcaMasterDecisionPoint::M1, // TODO differentiate between M1, M2, M3
                            StepsRemoved::new(0),        // TODO get steps removed from local clock
                        ))
                    } else {
                        BmcaRecommendation::Slave(ParentPortIdentity::new(
                            foreign_record.source_port_identity(),
                        ))
                    }
                } else {
                    BmcaRecommendation::Undecided
                }
            })
            .unwrap_or(BmcaRecommendation::Undecided);

        recommendation
    }
}

pub struct NoopBmca;

impl Bmca for NoopBmca {
    fn consider(
        &self,
        _source_port_identity: crate::port::PortIdentity,
        _announce: crate::message::AnnounceMessage,
    ) {
    }
    fn recommendation<C: crate::clock::SynchronizableClock>(
        &self,
        _local_clock: &LocalClock<C>,
    ) -> BmcaRecommendation {
        BmcaRecommendation::Undecided
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

    const CLK_QUALITY_HIGH: ClockQuality = ClockQuality::new(248, 0xFE, 0xFFFF);
    const CLK_QUALITY_MID: ClockQuality = ClockQuality::new(250, 0xFE, 0xFFFF);
    const CLK_QUALITY_LOW: ClockQuality = ClockQuality::new(255, 0xFF, 0xFFFF);

    impl ForeignClockDS {
        pub(crate) fn high_grade_test_clock() -> ForeignClockDS {
            ForeignClockDS::new(
                CLK_ID_HIGH,
                Priority1::new(127),
                Priority2::new(127),
                CLK_QUALITY_HIGH,
            )
        }

        pub(crate) fn mid_grade_test_clock() -> ForeignClockDS {
            ForeignClockDS::new(
                CLK_ID_MID,
                Priority1::new(127),
                Priority2::new(127),
                CLK_QUALITY_MID,
            )
        }

        pub(crate) fn low_grade_test_clock() -> ForeignClockDS {
            ForeignClockDS::new(
                CLK_ID_LOW,
                Priority1::new(127),
                Priority2::new(127),
                CLK_QUALITY_LOW,
            )
        }
    }

    impl LocalClockDS {
        pub(crate) fn high_grade_test_clock() -> LocalClockDS {
            LocalClockDS::new(
                CLK_ID_HIGH,
                Priority1::new(127),
                Priority2::new(127),
                CLK_QUALITY_HIGH,
            )
        }

        pub(crate) fn mid_grade_test_clock() -> LocalClockDS {
            LocalClockDS::new(
                CLK_ID_MID,
                Priority1::new(127),
                Priority2::new(127),
                CLK_QUALITY_MID,
            )
        }

        pub(crate) fn low_grade_test_clock() -> LocalClockDS {
            LocalClockDS::new(
                CLK_ID_LOW,
                Priority1::new(127),
                Priority2::new(127),
                CLK_QUALITY_LOW,
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

        assert!(high.outranks_other(&mid));
        assert!(!mid.outranks_other(&high));

        assert!(high.outranks_other(&low));
        assert!(!low.outranks_other(&high));

        assert!(mid.outranks_other(&low));
        assert!(!low.outranks_other(&mid));
    }

    #[test]
    fn test_foreign_clock_equality() {
        let c1 = ForeignClockDS::high_grade_test_clock();
        let c2 = ForeignClockDS::high_grade_test_clock();

        assert!(!c1.outranks_other(&c2));
        assert!(!c2.outranks_other(&c1));
        assert_eq!(c1, c2);
    }

    #[test]
    fn foreign_clock_ds_compares_priority1_before_quality() {
        // a has lower priority1 but worse quality; still outranks b.
        let a = ForeignClockDS::new(
            CLK_ID_LOW,
            Priority1::new(10),
            Priority2::new(127),
            CLK_QUALITY_LOW,
        );
        let b = ForeignClockDS::new(
            CLK_ID_HIGH,
            Priority1::new(100),
            Priority2::new(127),
            CLK_QUALITY_HIGH,
        );

        assert!(a.outranks_other(&b));
        assert!(!b.outranks_other(&a));
    }

    #[test]
    fn foreign_clock_ds_compares_priority2_after_quality() {
        // Same p1 and quality; lower p2 wins.
        let a = ForeignClockDS::new(
            CLK_ID_LOW,
            Priority1::new(127),
            Priority2::new(10),
            CLK_QUALITY_HIGH,
        );
        let b = ForeignClockDS::new(
            CLK_ID_HIGH,
            Priority1::new(127),
            Priority2::new(20),
            CLK_QUALITY_HIGH,
        );

        assert!(a.outranks_other(&b));
        assert!(!b.outranks_other(&a));
    }

    #[test]
    fn foreign_clock_ds_tiebreaks_on_identity_last() {
        // All fields equal except identity; lower identity outranks higher.
        let a = ForeignClockDS::new(
            CLK_ID_HIGH,
            Priority1::new(127),
            Priority2::new(127),
            CLK_QUALITY_HIGH,
        );
        let b = ForeignClockDS::new(
            CLK_ID_MID,
            Priority1::new(127),
            Priority2::new(127),
            CLK_QUALITY_HIGH,
        );

        assert!(a.outranks_other(&b));
        assert!(!b.outranks_other(&a));
    }

    #[test]
    fn full_bmca_recommends_slave_from_interleaved_announce_sequence() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::low_grade_test_clock());
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());

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

        let recommendation = bmca.recommendation(&local_clock);

        assert_eq!(
            recommendation,
            BmcaRecommendation::Slave(ParentPortIdentity::new(port_id_high),)
        );
    }

    #[test]
    fn full_bmca_recommends_slave_from_non_interleaved_announce_sequence() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::low_grade_test_clock());
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());

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

        let recommendation = bmca.recommendation(&local_clock);

        assert_eq!(
            recommendation,
            BmcaRecommendation::Slave(ParentPortIdentity::new(port_id_high),)
        );
    }

    #[test]
    fn full_bmca_undecided_when_no_announces_yet() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());

        assert_eq!(
            bmca.recommendation(&local_clock),
            BmcaRecommendation::Undecided
        );
    }

    #[test]
    fn full_bmca_undecided_when_no_qualified_clock_records_yet() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());

        let foreign_high = ForeignClockDS::high_grade_test_clock();

        bmca.consider(
            PortIdentity::fake(),
            AnnounceMessage::new(0.into(), foreign_high),
        );

        assert_eq!(
            bmca.recommendation(&local_clock),
            BmcaRecommendation::Undecided
        );
    }

    #[test]
    fn full_bmca_undecided_when_only_single_announces_each() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());

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

        assert_eq!(
            bmca.recommendation(&local_clock),
            BmcaRecommendation::Undecided
        );
    }

    #[test]
    fn bmca_undecided_on_sequence_gap() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());

        let foreign_high = ForeignClockDS::high_grade_test_clock();

        bmca.consider(
            PortIdentity::fake(),
            AnnounceMessage::new(0.into(), foreign_high),
        );
        bmca.consider(
            PortIdentity::fake(),
            AnnounceMessage::new(2.into(), foreign_high),
        );

        assert_eq!(
            bmca.recommendation(&local_clock),
            BmcaRecommendation::Undecided
        );
    }

    #[test]
    fn bmca_undecided_on_sequence_gap_after_being_qualified_before() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());
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
            bmca.recommendation(&local_clock),
            BmcaRecommendation::Slave(_)
        ));

        bmca.consider(
            PortIdentity::fake(),
            AnnounceMessage::new(3.into(), foreign_high),
        );

        assert_eq!(
            bmca.recommendation(&local_clock),
            BmcaRecommendation::Undecided
        );
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
