use crate::clock::{ClockIdentity, ClockQuality};
use crate::message::AnnounceMessage;

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

    pub fn announce(&self, sequence_id: u16) -> AnnounceMessage {
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

pub struct BestForeignClock<S: SortedForeignClockRecords> {
    sorted_clock_records: S,
}

impl<S: SortedForeignClockRecords> BestForeignClock<S> {
    pub fn new(sorted_clock_records: S) -> Self {
        Self {
            sorted_clock_records,
        }
    }

    pub fn consider(&mut self, announce: AnnounceMessage) {
        let foreign = announce.foreign_clock();

        let updated = self.sorted_clock_records.update_record(foreign, |record| {
            record.consider(announce);
        });

        if !updated {
            self.sorted_clock_records
                .insert(ForeignClockRecord::new(announce));
        }
    }

    pub fn clock(&self) -> Option<&ForeignClockDS> {
        self.sorted_clock_records.first()?.clock()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

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

        pub(crate) fn _low_grade_test_clock() -> LocalClockDS {
            LocalClockDS::new(CLK_ID_LOW, CLK_QUALITY_LOW)
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
    fn best_foreign_clock_yields_from_interleaved_announce_sequence() {
        let mut best_foreign_clock = BestForeignClock::new(SortedForeignClockRecordsVec::new());

        let foreign_high = ForeignClockDS::high_grade_test_clock();
        let foreign_mid = ForeignClockDS::mid_grade_test_clock();

        best_foreign_clock.consider(AnnounceMessage::new(0, foreign_high));
        best_foreign_clock.consider(AnnounceMessage::new(0, foreign_mid));
        best_foreign_clock.consider(AnnounceMessage::new(1, foreign_high));
        best_foreign_clock.consider(AnnounceMessage::new(1, foreign_mid));

        assert_eq!(best_foreign_clock.clock(), Some(&foreign_high));
    }

    #[test]
    fn best_foreign_clock_yields_from_segregated_announce_sequence() {
        let mut best_foreign_clock = BestForeignClock::new(SortedForeignClockRecordsVec::new());

        let foreign_high = ForeignClockDS::high_grade_test_clock();
        let foreign_mid = ForeignClockDS::mid_grade_test_clock();

        best_foreign_clock.consider(AnnounceMessage::new(0, foreign_high));
        best_foreign_clock.consider(AnnounceMessage::new(1, foreign_high));
        best_foreign_clock.consider(AnnounceMessage::new(0, foreign_mid));
        best_foreign_clock.consider(AnnounceMessage::new(1, foreign_mid));

        assert_eq!(best_foreign_clock.clock(), Some(&foreign_high));
    }

    #[test]
    fn best_foreign_clock_yields_none_when_no_announces() {
        let best_foreign_clock = BestForeignClock::new(SortedForeignClockRecordsVec::new());

        assert_eq!(best_foreign_clock.clock(), None);
    }

    #[test]
    fn best_foreign_clock_yields_none_when_no_clock_records() {
        let mut best_foreign_clock = BestForeignClock::new(SortedForeignClockRecordsVec::new());

        let foreign_high = ForeignClockDS::high_grade_test_clock();

        best_foreign_clock.consider(AnnounceMessage::new(0, foreign_high));

        assert_eq!(best_foreign_clock.clock(), None);
    }

    #[test]
    fn best_foreign_clock_yields_none_when_only_single_announces_each() {
        let mut best_foreign_clock = BestForeignClock::new(SortedForeignClockRecordsVec::new());

        let foreign_high = ForeignClockDS::high_grade_test_clock();
        let foreign_mid = ForeignClockDS::mid_grade_test_clock();
        let foreign_low = ForeignClockDS::low_grade_test_clock();

        best_foreign_clock.consider(AnnounceMessage::new(0, foreign_high));
        best_foreign_clock.consider(AnnounceMessage::new(5, foreign_mid));
        best_foreign_clock.consider(AnnounceMessage::new(10, foreign_low));

        assert_eq!(best_foreign_clock.clock(), None);
    }

    #[test]
    fn best_foreign_clock_yields_none_on_sequence_gap() {
        let mut best_foreign_clock = BestForeignClock::new(SortedForeignClockRecordsVec::new());

        let foreign_high = ForeignClockDS::high_grade_test_clock();

        best_foreign_clock.consider(AnnounceMessage::new(0, foreign_high));
        best_foreign_clock.consider(AnnounceMessage::new(2, foreign_high));

        assert_eq!(best_foreign_clock.clock(), None);
    }

    #[test]
    fn best_foreign_clock_unqualifies_on_sequence_gap() {
        let mut best_foreign = BestForeignClock::new(SortedForeignClockRecordsVec::new());
        let foreign_high = ForeignClockDS::high_grade_test_clock();

        best_foreign.consider(AnnounceMessage::new(0, foreign_high));
        best_foreign.consider(AnnounceMessage::new(1, foreign_high));
        best_foreign.consider(AnnounceMessage::new(3, foreign_high));

        assert_eq!(best_foreign.clock(), None);
    }
}
