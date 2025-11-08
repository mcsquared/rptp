use std::cell::RefCell;

use crate::clock::{ClockIdentity, ClockQuality, LocalClock, SynchronizableClock};
use crate::message::{AnnounceMessage, SequenceId};

pub trait SortedForeignClockRecords {
    fn insert(&mut self, record: ForeignClockRecord);
    fn update_record<F>(&mut self, foreign: &ForeignClockDS, update: F) -> bool
    where
        F: FnOnce(&mut ForeignClockRecord);
    fn first(&self) -> Option<&ForeignClockRecord>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForeignClockDS {
    identity: ClockIdentity,
    quality: ClockQuality,
}

impl ForeignClockDS {
    pub fn new(identity: ClockIdentity, quality: ClockQuality) -> Self {
        Self { identity, quality }
    }

    pub fn same_source_as(&self, other: &ForeignClockDS) -> bool {
        self.identity == other.identity
    }

    pub fn outranks_other(&self, other: &ForeignClockDS) -> bool {
        if self.identity == other.identity {
            return false;
        }

        self.quality.outranks_other(&other.quality)
    }
}

pub struct LocalClockDS {
    ds: ForeignClockDS,
}

impl LocalClockDS {
    pub fn new(identity: ClockIdentity, quality: ClockQuality) -> Self {
        Self {
            ds: ForeignClockDS::new(identity, quality),
        }
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
    last_announce: AnnounceMessage,
    foreign_clock: Option<ForeignClockDS>,
}

impl ForeignClockRecord {
    pub fn new(announce: AnnounceMessage) -> Self {
        Self {
            last_announce: announce,
            foreign_clock: None,
        }
    }

    pub fn same_source_as(&self, other: &ForeignClockDS) -> bool {
        self.last_announce.foreign_clock().same_source_as(other)
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
}

impl Ord for ForeignClockRecord {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering::*;

        match (self.clock(), other.clock()) {
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
        }
    }
}

impl PartialOrd for ForeignClockRecord {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BmcaRecommendation {
    Undecided,
    Master,
    Slave,
    // TODO: Passive,
}

pub trait Bmca {
    fn consider(&self, announce: AnnounceMessage);
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
    fn consider(&self, announce: AnnounceMessage) {
        let foreign = announce.foreign_clock();

        let updated = self
            .sorted_clock_records
            .borrow_mut()
            .update_record(foreign, |record| {
                record.consider(announce);
            });

        if !updated {
            self.sorted_clock_records
                .borrow_mut()
                .insert(ForeignClockRecord::new(announce));
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
                        BmcaRecommendation::Master
                    } else {
                        BmcaRecommendation::Slave
                    }
                } else {
                    BmcaRecommendation::Undecided
                }
            })
            .unwrap_or(BmcaRecommendation::Undecided);

        recommendation
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    use crate::clock::FakeClock;
    use crate::infra::infra_support::SortedForeignClockRecordsVec;

    const CLK_ID_HIGH: ClockIdentity =
        ClockIdentity::new([0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x01]);
    const CLK_ID_MID: ClockIdentity =
        ClockIdentity::new([0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x02]);
    const CLK_ID_LOW: ClockIdentity =
        ClockIdentity::new([0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x03]);

    const CLK_QUALITY_HIGH: ClockQuality = ClockQuality::new(248, 0xFE, 0xFFFF);
    const CLK_QUALITY_MID: ClockQuality = ClockQuality::new(250, 0xFE, 0xFFFF);
    const CLK_QUALITY_LOW: ClockQuality = ClockQuality::new(255, 0xFF, 0xFFFF);

    impl ForeignClockDS {
        pub(crate) fn high_grade_test_clock() -> ForeignClockDS {
            ForeignClockDS::new(CLK_ID_HIGH, CLK_QUALITY_HIGH)
        }

        pub(crate) fn mid_grade_test_clock() -> ForeignClockDS {
            ForeignClockDS::new(CLK_ID_MID, CLK_QUALITY_MID)
        }

        pub(crate) fn low_grade_test_clock() -> ForeignClockDS {
            ForeignClockDS::new(CLK_ID_LOW, CLK_QUALITY_LOW)
        }
    }

    impl LocalClockDS {
        pub(crate) fn high_grade_test_clock() -> LocalClockDS {
            LocalClockDS::new(CLK_ID_HIGH, CLK_QUALITY_HIGH)
        }

        pub(crate) fn mid_grade_test_clock() -> LocalClockDS {
            LocalClockDS::new(CLK_ID_MID, CLK_QUALITY_MID)
        }

        pub(crate) fn low_grade_test_clock() -> LocalClockDS {
            LocalClockDS::new(CLK_ID_LOW, CLK_QUALITY_LOW)
        }
    }

    impl ForeignClockRecord {
        pub(crate) fn with_resolved_clock(self, foreign_clock: ForeignClockDS) -> Self {
            assert!(self.same_source_as(&foreign_clock));

            Self {
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
    fn full_bmca_recommends_slave_from_interleaved_announce_sequence() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::low_grade_test_clock());
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());

        let foreign_high = ForeignClockDS::high_grade_test_clock();
        let foreign_mid = ForeignClockDS::mid_grade_test_clock();

        bmca.consider(AnnounceMessage::new(0.into(), foreign_high));
        bmca.consider(AnnounceMessage::new(0.into(), foreign_mid));
        bmca.consider(AnnounceMessage::new(1.into(), foreign_high));
        bmca.consider(AnnounceMessage::new(1.into(), foreign_mid));

        assert_eq!(bmca.recommendation(&local_clock), BmcaRecommendation::Slave);
    }

    #[test]
    fn full_bmca_recommends_slave_from_non_interleaved_announce_sequence() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::low_grade_test_clock());
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());

        let foreign_high = ForeignClockDS::high_grade_test_clock();
        let foreign_mid = ForeignClockDS::mid_grade_test_clock();

        bmca.consider(AnnounceMessage::new(0.into(), foreign_high));
        bmca.consider(AnnounceMessage::new(1.into(), foreign_high));
        bmca.consider(AnnounceMessage::new(0.into(), foreign_mid));
        bmca.consider(AnnounceMessage::new(1.into(), foreign_mid));

        assert_eq!(bmca.recommendation(&local_clock), BmcaRecommendation::Slave);
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

        bmca.consider(AnnounceMessage::new(0.into(), foreign_high));

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

        bmca.consider(AnnounceMessage::new(0.into(), foreign_high));
        bmca.consider(AnnounceMessage::new(5.into(), foreign_mid));
        bmca.consider(AnnounceMessage::new(10.into(), foreign_low));

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

        bmca.consider(AnnounceMessage::new(0.into(), foreign_high));
        bmca.consider(AnnounceMessage::new(2.into(), foreign_high));

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

        bmca.consider(AnnounceMessage::new(0.into(), foreign_high));
        bmca.consider(AnnounceMessage::new(1.into(), foreign_high));

        assert_eq!(bmca.recommendation(&local_clock), BmcaRecommendation::Slave);

        bmca.consider(AnnounceMessage::new(3.into(), foreign_high));

        assert_eq!(
            bmca.recommendation(&local_clock),
            BmcaRecommendation::Undecided
        );
    }
}
