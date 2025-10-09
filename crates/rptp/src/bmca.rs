use std::cell::RefCell;

use crate::clock::{ClockIdentity, ClockQuality};
use crate::message::AnnounceMessage;

pub trait SortedForeignClockRecords {
    fn insert(&mut self, record: ForeignClockRecord);
    fn update_record<F>(&mut self, foreign: &ForeignClock, update: F) -> bool
    where
        F: FnOnce(&mut ForeignClockRecord);
    fn first(&self) -> Option<&ForeignClockRecord>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForeignClock {
    identity: ClockIdentity,
    quality: ClockQuality,
}

impl ForeignClock {
    pub fn new(identity: ClockIdentity, quality: ClockQuality) -> Self {
        Self { identity, quality }
    }

    pub fn same_source_as(&self, other: &ForeignClock) -> bool {
        self.identity == other.identity
    }

    pub fn outranks_other(&self, other: &ForeignClock) -> bool {
        if self.identity == other.identity {
            return false;
        }

        self.quality.outranks_other(&other.quality)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForeignClockRecord {
    last_announce: AnnounceMessage,
    foreign_clock: Option<ForeignClock>,
}

impl ForeignClockRecord {
    pub fn new(announce: AnnounceMessage) -> Self {
        Self {
            last_announce: announce,
            foreign_clock: None,
        }
    }

    pub fn same_source_as(&self, other: &ForeignClock) -> bool {
        self.last_announce.foreign_clock().same_source_as(other)
    }

    pub fn consider(&mut self, announce: AnnounceMessage) -> Option<&ForeignClock> {
        if let Some(clock) = announce.follows(self.last_announce) {
            self.foreign_clock = Some(clock);
        }
        self.last_announce = announce;
        self.foreign_clock.as_ref()
    }

    pub fn clock(&self) -> Option<&ForeignClock> {
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
    sorted_clock_records: RefCell<S>,
}

impl<S: SortedForeignClockRecords> BestForeignClock<S> {
    pub fn new(sorted_clock_records: S) -> Self {
        Self {
            sorted_clock_records: RefCell::new(sorted_clock_records),
        }
    }

    pub fn consider(&self, announce: AnnounceMessage) {
        let foreign = announce.foreign_clock();
        let mut records = self.sorted_clock_records.borrow_mut();

        let updated = records.update_record(foreign, |record| {
            record.consider(announce);
        });

        if !updated {
            records.insert(ForeignClockRecord::new(announce));
        }
    }

    pub fn clock(&self) -> Option<ForeignClock> {
        self.sorted_clock_records
            .borrow()
            .first()
            .and_then(|record| record.clock().copied())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    use crate::infra::infra_support::SortedForeignClockRecordsVec;

    impl ForeignClock {
        pub(crate) fn high_grade_test_clock() -> ForeignClock {
            ForeignClock::new(
                ClockIdentity::new([0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
                ClockQuality::new(248, 0xFE, 0xFFFF),
            )
        }

        pub(crate) fn mid_grade_test_clock() -> ForeignClock {
            ForeignClock::new(
                ClockIdentity::new([0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x02]),
                ClockQuality::new(250, 0xFE, 0xFFFF),
            )
        }

        pub(crate) fn low_grade_test_clock() -> ForeignClock {
            ForeignClock::new(
                ClockIdentity::new([0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x03]),
                ClockQuality::new(255, 0xFF, 0xFFFF),
            )
        }
    }

    #[test]
    fn test_foreign_clock_ordering() {
        let high = ForeignClock::high_grade_test_clock();
        let mid = ForeignClock::mid_grade_test_clock();
        let low = ForeignClock::low_grade_test_clock();

        assert!(high.outranks_other(&mid));
        assert!(!mid.outranks_other(&high));

        assert!(high.outranks_other(&low));
        assert!(!low.outranks_other(&high));

        assert!(mid.outranks_other(&low));
        assert!(!low.outranks_other(&mid));
    }

    #[test]
    fn test_foreign_clock_equality() {
        let c1 = ForeignClock::high_grade_test_clock();
        let c2 = ForeignClock::high_grade_test_clock();

        assert!(!c1.outranks_other(&c2));
        assert!(!c2.outranks_other(&c1));
        assert_eq!(c1, c2);
    }

    #[test]
    fn best_foreign_clock_yields_from_interleaved_announce_sequence() {
        let best_foreign_clock = BestForeignClock::new(SortedForeignClockRecordsVec::new());

        let foreign_high = ForeignClock::high_grade_test_clock();
        let foreign_mid = ForeignClock::mid_grade_test_clock();

        best_foreign_clock.consider(AnnounceMessage::new(0, foreign_high));
        best_foreign_clock.consider(AnnounceMessage::new(0, foreign_mid));
        best_foreign_clock.consider(AnnounceMessage::new(1, foreign_high));
        best_foreign_clock.consider(AnnounceMessage::new(1, foreign_mid));

        assert_eq!(best_foreign_clock.clock(), Some(foreign_high));
    }

    #[test]
    fn best_foreign_clock_yields_from_segregated_announce_sequence() {
        let best_foreign_clock = BestForeignClock::new(SortedForeignClockRecordsVec::new());

        let foreign_high = ForeignClock::high_grade_test_clock();
        let foreign_mid = ForeignClock::mid_grade_test_clock();

        best_foreign_clock.consider(AnnounceMessage::new(0, foreign_high));
        best_foreign_clock.consider(AnnounceMessage::new(1, foreign_high));
        best_foreign_clock.consider(AnnounceMessage::new(0, foreign_mid));
        best_foreign_clock.consider(AnnounceMessage::new(1, foreign_mid));

        assert_eq!(best_foreign_clock.clock(), Some(foreign_high));
    }

    #[test]
    fn best_foreign_clock_yields_none_when_no_announces() {
        let best_foreign_clock = BestForeignClock::new(SortedForeignClockRecordsVec::new());

        assert_eq!(best_foreign_clock.clock(), None);
    }

    #[test]
    fn best_foreign_clock_yields_none_when_no_clock_records() {
        let best_foreign_clock = BestForeignClock::new(SortedForeignClockRecordsVec::new());

        let foreign_high = ForeignClock::high_grade_test_clock();

        best_foreign_clock.consider(AnnounceMessage::new(0, foreign_high));

        assert_eq!(best_foreign_clock.clock(), None);
    }

    #[test]
    fn best_foreign_clock_yields_none_when_only_single_announces_each() {
        let best_foreign_clock = BestForeignClock::new(SortedForeignClockRecordsVec::new());

        let foreign_high = ForeignClock::high_grade_test_clock();
        let foreign_mid = ForeignClock::mid_grade_test_clock();
        let foreign_low = ForeignClock::low_grade_test_clock();

        best_foreign_clock.consider(AnnounceMessage::new(0, foreign_high));
        best_foreign_clock.consider(AnnounceMessage::new(5, foreign_mid));
        best_foreign_clock.consider(AnnounceMessage::new(10, foreign_low));

        assert_eq!(best_foreign_clock.clock(), None);
    }

    #[test]
    fn best_foreign_clock_yields_none_when_gaps_in_sequence() {
        let best_foreign_clock = BestForeignClock::new(SortedForeignClockRecordsVec::new());

        let foreign_high = ForeignClock::high_grade_test_clock();

        best_foreign_clock.consider(AnnounceMessage::new(0, foreign_high));
        best_foreign_clock.consider(AnnounceMessage::new(2, foreign_high));

        assert_eq!(best_foreign_clock.clock(), None);
    }
}
