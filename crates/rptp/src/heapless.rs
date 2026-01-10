use heapless::Vec;

use crate::bmca::{ForeignClockRecord, ForeignClockStatus, ForeignClockRecords};

/// Heapless implementation of [`ForeignClockRecords`] backed by a bounded
/// `heapless::Vec`.
///
/// This type is intended for embedded adopters that want a ready‑made fixed‑capacity
/// foreign clock store. Applications that prefer to supply their own storage can
/// ignore this type and implement [`ForeignClockRecords`] on their own.
pub struct HeaplessForeignClockRecords<const N: usize> {
    records: Vec<ForeignClockRecord, N>,
    removal_policy: RemovalPolicy,
}

impl<const N: usize> HeaplessForeignClockRecords<N> {
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
            removal_policy: RemovalPolicy,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    #[allow(dead_code)]
    pub(crate) fn from_records(records: &[ForeignClockRecord]) -> Self {
        let mut vec = Vec::from_slice(records).unwrap_or_default();
        vec.sort_unstable();
        Self {
            records: vec,
            removal_policy: RemovalPolicy,
        }
    }
}

impl Default for HeaplessForeignClockRecords<0> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> ForeignClockRecords for HeaplessForeignClockRecords<N> {
    fn remember(&mut self, record: ForeignClockRecord) {
        if let Some(existing) = self
            .records
            .iter_mut()
            .find(|r| r.same_source_as(record.source_port_identity()))
        {
            if let ForeignClockStatus::Updated = existing.update_from(&record) {
                self.records.sort_unstable();
            }
            return;
        }

        let record = match self.records.push(record) {
            Ok(()) => {
                self.records.sort_unstable();
                return;
            }
            Err(record) => record,
        };

        // Capacity exceeded, determine if there is a record to remove by policy
        if let Some(index) = self.removal_policy.candidate_index(&self.records, &record) {
            self.records.remove(index);
            let result = self.records.push(record);
            assert!(result.is_ok());

            self.records.sort_unstable();
        }
    }

    fn best_qualified(&self) -> Option<&ForeignClockRecord> {
        self.records
            .first()
            .filter(|record| record.qualified_ds().is_some())
    }

    fn prune_stale(&mut self, now: crate::time::Instant) -> bool {
        let before = self.records.len();
        self.records.retain(|record| !record.is_stale(now));
        self.records.len() != before
    }
}

// Replacement policy to determine which record to possibly remove when capacity is
// exceeded to make room for a new, possibly better record, even if unqualified.
struct RemovalPolicy;

impl RemovalPolicy {
    fn candidate_index(
        &self,
        current: &[ForeignClockRecord],
        candidate: &ForeignClockRecord,
    ) -> Option<usize> {
        current.iter().rev().enumerate().find_map(|(i, record)| {
            if candidate.better_by_dataset_than(record) {
                Some(current.len() - 1 - i)
            } else {
                None
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ClockIdentity;
    use crate::clock::StepsRemoved;
    use crate::port::{PortIdentity, PortNumber};
    use crate::test_support::TestClockCatalog;
    use crate::time::{Instant, LogInterval};

    fn new_port_identity(last_octet: u8) -> PortIdentity {
        PortIdentity::new(
            ClockIdentity::new(&[0, 1, 2, 3, 4, 5, 6, last_octet]),
            PortNumber::new(1),
        )
    }

    #[test]
    fn heapless_maintains_best_record_first() {
        let high_clock = TestClockCatalog::default_high_grade().foreign_ds(StepsRemoved::new(0));
        let mid_clock = TestClockCatalog::default_mid_grade().foreign_ds(StepsRemoved::new(0));
        let low_clock =
            TestClockCatalog::default_low_grade_slave_only().foreign_ds(StepsRemoved::new(0));

        let high_port_id = new_port_identity(1);
        let mid_port_id = new_port_identity(2);
        let low_port_id = new_port_identity(3);

        let records = HeaplessForeignClockRecords::<4>::from_records(&[
            ForeignClockRecord::qualified(
                high_port_id,
                high_clock,
                LogInterval::new(0),
                Instant::from_secs(0),
            ),
            ForeignClockRecord::new(
                low_port_id,
                low_clock,
                LogInterval::new(0),
                Instant::from_secs(0),
            ),
            ForeignClockRecord::new(
                mid_port_id,
                mid_clock,
                LogInterval::new(0),
                Instant::from_secs(0),
            ),
        ]);

        let best_clock = records
            .best_qualified()
            .and_then(|record| record.qualified_ds());
        assert_eq!(best_clock, Some(&high_clock));
    }

    fn same_dataset(a: &ForeignClockRecord, b: &ForeignClockRecord) -> bool {
        !a.better_by_dataset_than(b) && !b.better_by_dataset_than(a)
    }

    #[test]
    fn heapless_replacement_keeps_best_records_on_overflow() {
        let high_clock = TestClockCatalog::default_high_grade().foreign_ds(StepsRemoved::new(0));
        let mid_clock = TestClockCatalog::default_mid_grade().foreign_ds(StepsRemoved::new(0));
        let low_clock =
            TestClockCatalog::default_low_grade_slave_only().foreign_ds(StepsRemoved::new(0));

        let high_port_id = new_port_identity(1);
        let mid_port_id = new_port_identity(2);
        let low_port_id = new_port_identity(3);

        // Start with mid and low, both qualified.
        let mut records = HeaplessForeignClockRecords::<2>::from_records(&[
            ForeignClockRecord::qualified(
                mid_port_id,
                mid_clock,
                LogInterval::new(0),
                Instant::from_secs(0),
            ),
            ForeignClockRecord::qualified(
                low_port_id,
                low_clock,
                LogInterval::new(0),
                Instant::from_secs(0),
            ),
        ]);

        // Insert a better clock; capacity is exceeded, so low should be removed.
        records.remember(ForeignClockRecord::new(
            high_port_id,
            high_clock,
            LogInterval::new(0),
            Instant::from_secs(0),
        ));

        assert_eq!(records.records.len(), 2);

        let template_high = ForeignClockRecord::new(
            high_port_id,
            high_clock,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        let template_mid = ForeignClockRecord::new(
            mid_port_id,
            mid_clock,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        let template_low = ForeignClockRecord::new(
            low_port_id,
            low_clock,
            LogInterval::new(0),
            Instant::from_secs(0),
        );

        let has_high = records
            .records
            .iter()
            .any(|r| same_dataset(r, &template_high));
        let has_mid = records
            .records
            .iter()
            .any(|r| same_dataset(r, &template_mid));
        let has_low = records
            .records
            .iter()
            .any(|r| same_dataset(r, &template_low));

        assert!(has_high);
        assert!(has_mid);
        assert!(!has_low);
    }

    #[test]
    fn heapless_replacement_does_not_replace_when_candidate_is_worse() {
        let high_clock = TestClockCatalog::default_high_grade().foreign_ds(StepsRemoved::new(0));
        let mid_clock = TestClockCatalog::default_mid_grade().foreign_ds(StepsRemoved::new(0));
        let low_clock =
            TestClockCatalog::default_low_grade_slave_only().foreign_ds(StepsRemoved::new(0));

        let high_port_id = new_port_identity(1);
        let mid_port_id = new_port_identity(2);
        let low_port_id = new_port_identity(3);

        // Start with high and mid, both qualified.
        let mut records = HeaplessForeignClockRecords::<2>::from_records(&[
            ForeignClockRecord::qualified(
                high_port_id,
                high_clock,
                LogInterval::new(0),
                Instant::from_secs(0),
            ),
            ForeignClockRecord::qualified(
                mid_port_id,
                mid_clock,
                LogInterval::new(0),
                Instant::from_secs(0),
            ),
        ]);

        // Insert a worse clock; capacity is exceeded, but no record should be replaced.
        records.remember(ForeignClockRecord::new(
            low_port_id,
            low_clock,
            LogInterval::new(0),
            Instant::from_secs(0),
        ));

        assert_eq!(records.records.len(), 2);

        let template_high = ForeignClockRecord::new(
            high_port_id,
            high_clock,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        let template_mid = ForeignClockRecord::new(
            mid_port_id,
            mid_clock,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        let template_low = ForeignClockRecord::new(
            low_port_id,
            low_clock,
            LogInterval::new(0),
            Instant::from_secs(0),
        );

        let has_high = records
            .records
            .iter()
            .any(|r| same_dataset(r, &template_high));
        let has_mid = records
            .records
            .iter()
            .any(|r| same_dataset(r, &template_mid));
        let has_low = records
            .records
            .iter()
            .any(|r| same_dataset(r, &template_low));

        assert!(has_high);
        assert!(has_mid);
        assert!(!has_low);
    }

    #[test]
    fn heapless_prune_stale_returns_true_when_records_removed() {
        let high_clock = TestClockCatalog::default_high_grade().foreign_ds(StepsRemoved::new(0));
        let high_port_id = new_port_identity(1);

        let mut records = HeaplessForeignClockRecords::<4>::new();
        records.remember(ForeignClockRecord::new(
            high_port_id,
            high_clock,
            LogInterval::new(0),
            Instant::from_secs(0),
        ));

        // With logInterval = 0, the foreign master time window is 4 seconds.
        // Advancing well beyond that should mark the record as stale.
        let pruned = records.prune_stale(Instant::from_secs(10));

        assert!(pruned, "prune_stale should report removal of stale records");
        assert!(
            records.records.is_empty(),
            "all stale records should have been removed"
        );
    }

    #[test]
    fn heapless_prune_stale_returns_false_when_no_records_are_stale() {
        let high_clock = TestClockCatalog::default_high_grade().foreign_ds(StepsRemoved::new(0));
        let high_port_id = new_port_identity(1);

        let mut records = HeaplessForeignClockRecords::<4>::new();
        records.remember(ForeignClockRecord::new(
            high_port_id,
            high_clock,
            LogInterval::new(0),
            Instant::from_secs(0),
        ));

        // Within the 4-second window, the record is not yet stale.
        let pruned = records.prune_stale(Instant::from_secs(2));

        assert!(
            !pruned,
            "prune_stale should report no removals when nothing is stale"
        );
        assert_eq!(records.records.len(), 1);
    }
}
