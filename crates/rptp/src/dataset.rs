//! BMCA comparison data set and wire format.
//!
//! This module defines the vocabulary used when comparing local and foreign clocks in the
//! Best Master Clock Algorithm (BMCA):
//!
//! - **[`ClockDS`]** – The data set describing a grandmaster candidate (identity,
//!   priority1, priority2, quality, steps_removed). It corresponds to the fields carried in
//!   Announce messages and used by BMCA when comparing clocks. [`ClockDS::better_than`] encodes
//!   the BMCA ranking order (priority1, then quality, then priority2, then identity, then
//!   steps_removed).
//! - **[`Priority1`]**, **[`Priority2`]**, **[`StepsRemoved`]** – Dataset field types used in
//!   comparisons and in the wire representation.
//!
//! The module also provides the 16-byte BMCA comparison tuple wire format ([`ClockDS::from_wire`],
//! [`ClockDS::to_wire`]) as used in PTP Announce payloads.
//!
//! The BMCA algorithm and roles that *use* these types live in [`crate::bmca`]. Foreign master
//! tracking and qualification (which hold and qualify [`ClockDS`] from Announce messages) live in
//! [`crate::foreign`].

use core::ops::Range;

use crate::clock::{ClockIdentity, ClockQuality};
use crate::foreign::BestForeignSnapshot;

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

    /// Return `true` if this dataset is strictly better than `other`
    /// according to BMCA ranking rules.
    pub(crate) fn better_than(&self, other: &ClockDS) -> bool {
        if self.identity == other.identity {
            return self.steps_removed.better_than(&other.steps_removed);
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

    pub(crate) fn better_than_foreign(&self, other: &BestForeignSnapshot) -> bool {
        match other {
            BestForeignSnapshot::Qualified { ds, .. } => self.better_than(ds),
            BestForeignSnapshot::Empty => true,
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

    pub(crate) fn better_than(&self, other: &StepsRemoved) -> bool {
        self.0.saturating_add(1) < other.0
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

    #[test]
    fn clock_ds_ordering() {
        let high = TestClockDS::default_high_grade().dataset();
        let mid = TestClockDS::default_mid_grade().dataset();
        let low = TestClockDS::default_low_grade_slave_only().dataset();

        assert!(high.better_than(&mid));
        assert!(!mid.better_than(&high));

        assert!(high.better_than(&low));
        assert!(!low.better_than(&high));

        assert!(mid.better_than(&low));
        assert!(!low.better_than(&mid));
    }

    #[test]
    fn clock_ds_equality() {
        let c1 = TestClockDS::default_high_grade().dataset();
        let c2 = TestClockDS::default_high_grade().dataset();

        assert!(!c1.better_than(&c2));
        assert!(!c2.better_than(&c1));
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
        assert!(a.better_than(&b));
        assert!(!b.better_than(&a));
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
        assert!(a.better_than(&b));
        assert!(!b.better_than(&a));
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
        assert!(a.better_than(&b));
        assert!(!b.better_than(&a));
    }

    #[test]
    fn clock_ds_prefers_lower_steps_removed_when_identity_equal() {
        let a = TestClockDS::default_high_grade()
            .with_steps_removed(StepsRemoved::new(1))
            .dataset();
        let b = TestClockDS::default_high_grade()
            .with_steps_removed(StepsRemoved::new(3))
            .dataset();

        assert!(a.better_than(&b));
        assert!(!b.better_than(&a));
    }
}
