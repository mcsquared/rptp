//! Time representations used in the `rptp` domain.
//!
//! `rptp` distinguishes between:
//! - [`TimeStamp`]: a PTP timestamp in seconds + nanoseconds with the IEEE 1588 wire-range
//!   restriction (48-bit seconds field).
//! - [`TimeInterval`]: a signed duration/offset between timestamps, used for `offsetFromMaster`,
//!   delay/offset computations, and servo inputs.
//! - [`Instant`] / [`Duration`]: monotonic, local time primitives used for scheduling and timeout
//!   logic (independent of the PTP time scale).
//! - [`LogInterval`] / [`LogMessageInterval`]: IEEE 1588 “log2 intervals” used in datasets and
//!   message fields.

/// A PTP timestamp (seconds + nanoseconds) with IEEE 1588 wire-range bounds.
///
/// The on-the-wire timestamp format uses a 48-bit seconds field plus a 32-bit nanoseconds field.
/// `rptp` models this directly and enforces:
/// - `seconds < 2^48`, and
/// - `nanos < 1_000_000_000`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TimeStamp {
    seconds: u64,
    nanos: u32,
}

impl TimeStamp {
    const MAX_SECONDS_EXCL: u64 = 1 << 48;
    const NANOS_PER_SECOND: u32 = 1_000_000_000;

    /// Construct a new timestamp, panicking if it exceeds the IEEE 1588 wire bounds.
    ///
    /// That the constructor may panic due those invariant-checking assertions is part of `rptp`'s
    /// current fail-fast approach: `TimeStamp` is a domain object and values outside its invariants
    /// are considered unrecoverable (they cannot participate meaningfully in the domain).
    pub fn new(seconds: u64, nanos: u32) -> Self {
        assert!(seconds < Self::MAX_SECONDS_EXCL);
        assert!(nanos < 1_000_000_000);
        Self { seconds, nanos }
    }

    /// Fallible constructor for callers that naturally operate on signed seconds.
    ///
    /// Returns `None` if the resulting timestamp would be negative or outside the IEEE 1588
    /// representable range.
    pub fn try_new_i64(seconds: i64, nanos: u32) -> Option<Self> {
        if (0..Self::MAX_SECONDS_EXCL as i64).contains(&seconds) && nanos < Self::NANOS_PER_SECOND {
            Some(Self {
                seconds: seconds as u64,
                nanos,
            })
        } else {
            None
        }
    }

    /// Encode this timestamp into the IEEE 1588 on-the-wire 10-byte representation.
    pub(crate) fn to_wire(self) -> [u8; 10] {
        let mut buf = [0; 10];
        buf[0..2].copy_from_slice(&((self.seconds >> 32) as u16).to_be_bytes());
        buf[2..6].copy_from_slice(&(self.seconds as u32).to_be_bytes());
        buf[6..10].copy_from_slice(&self.nanos.to_be_bytes());
        buf
    }

    /// Add a signed interval to this timestamp, returning `None` on overflow or underflow.
    ///
    /// This operation is range-checked against the IEEE 1588 timestamp bounds.
    pub fn checked_add(self, rhs: TimeInterval) -> Option<Self> {
        let mut seconds = (self.seconds as i64).checked_add(rhs.seconds)?;
        let mut nanos = self.nanos + rhs.nanos;

        if nanos >= Self::NANOS_PER_SECOND {
            nanos -= Self::NANOS_PER_SECOND;
            seconds = seconds.checked_add(1)?;
        }

        TimeStamp::try_new_i64(seconds, nanos)
    }

    /// Subtract a signed interval from this timestamp, returning `None` on overflow or underflow.
    ///
    /// This operation is range-checked against the IEEE 1588 timestamp bounds.
    pub fn checked_sub(self, rhs: TimeInterval) -> Option<Self> {
        let mut seconds = (self.seconds as i64).checked_sub(rhs.seconds)?;
        let mut nanos = self.nanos as i32 - rhs.nanos as i32;

        if nanos < 0 {
            seconds = seconds.checked_sub(1)?;
            nanos += 1_000_000_000;
        }

        TimeStamp::try_new_i64(seconds, nanos as u32)
    }
}

impl core::ops::Sub for TimeStamp {
    type Output = TimeInterval;

    /// Compute the signed interval `self - rhs`.
    fn sub(self, rhs: Self) -> Self::Output {
        let mut delta_seconds = self.seconds as i64 - rhs.seconds as i64;
        let mut delta_nanos = self.nanos as i32 - rhs.nanos as i32;

        if delta_nanos < 0 {
            delta_seconds -= 1;
            delta_nanos += 1_000_000_000;
        }

        TimeInterval::new(delta_seconds, delta_nanos as u32)
    }
}

/// A signed time interval represented as seconds + fractional nanoseconds.
///
/// `TimeInterval` is used for offsets and durations in the PTP domain (e.g. `offsetFromMaster`).
/// It supports negative values.
///
/// Representation invariant:
/// - `nanos < 1_000_000_000`
/// - the sign is carried by `seconds` while `nanos` stores the non-negative fractional part.
///
/// For example, `TimeInterval::new(-1, 700_000_000)` represents `-0.3s` (i.e. `-1 + 0.7`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TimeInterval {
    seconds: i64,
    nanos: u32,
}

impl TimeInterval {
    /// Zero-length interval.
    pub const ZERO: Self = Self {
        seconds: 0,
        nanos: 0,
    };

    /// Construct an interval, panicking if `nanos` is out of range.
    ///
    /// Note: `seconds == i64::MIN` is only permitted with `nanos > 0` to keep `abs()` well-defined
    /// without overflowing on negation.
    ///
    /// That the constructor may panic due those invariant-checking assertions is part of `rptp`'s
    /// current fail-fast approach: `TimeInterval` is a domain object and values that violate its
    /// invariants are considered unrecoverable (they cannot participate meaningfully in the domain).
    pub fn new(seconds: i64, nanos: u32) -> Self {
        // Allow i64::MIN only with non-zero nanoseconds
        assert!(seconds != i64::MIN || nanos > 0);
        assert!(nanos < 1_000_000_000);
        Self { seconds, nanos }
    }

    /// Construct an interval from an unsigned nanosecond count.
    pub fn from_u64_nanos(nanos: u64) -> Self {
        let seconds = (nanos / 1_000_000_000) as i64;
        let nanos = (nanos % 1_000_000_000) as u32;
        Self::new(seconds, nanos)
    }

    fn total_nanos(&self) -> i128 {
        (self.seconds as i128 * 1_000_000_000) + self.nanos as i128
    }

    fn from_total_nanos(total: i128) -> Self {
        let seconds = total.div_euclid(1_000_000_000);
        let nanos = total.rem_euclid(1_000_000_000);
        debug_assert!(seconds >= i64::MIN as i128 && seconds <= i64::MAX as i128);
        Self::new(seconds as i64, nanos as u32)
    }

    /// Return half of this interval, rounding towards zero.
    pub fn half(self) -> Self {
        Self::from_total_nanos(self.total_nanos() / 2)
    }

    /// Return the absolute value of this interval.
    ///
    /// This preserves the `TimeInterval` representation invariant for negative values by
    /// converting `(-s, nanos)` into the corresponding positive interval.
    pub fn abs(&self) -> Self {
        if self.seconds < 0 {
            if self.nanos == 0 {
                Self::new(-self.seconds, 0)
            } else {
                let secs = self.seconds + 1;
                Self::new(-secs, 1_000_000_000 - self.nanos)
            }
        } else {
            *self
        }
    }

    /// Return this interval as fractional seconds in `f64` precision.
    ///
    /// The conversion is lossy for very large magnitudes but sufficient for
    /// logging, metrics and servo calculations.
    pub fn as_f64_seconds(&self) -> f64 {
        self.total_nanos() as f64 / 1_000_000_000.0
    }
}

impl core::ops::Sub for TimeInterval {
    type Output = TimeInterval;

    /// Subtract two signed intervals (`self - rhs`).
    fn sub(self, rhs: Self) -> Self::Output {
        let mut delta_seconds = self.seconds - rhs.seconds;
        let mut delta_nanos = self.nanos as i32 - rhs.nanos as i32;

        if delta_nanos < 0 {
            delta_seconds -= 1;
            delta_nanos += 1_000_000_000;
        }

        TimeInterval::new(delta_seconds, delta_nanos as u32)
    }
}

/// A monotonic time instant used for internal scheduling.
///
/// `Instant` is independent of PTP time. It is used for time-window logic (e.g. “stale after
/// N seconds”) and is expected to be backed by a monotonic clock in infrastructure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Instant {
    nanos: u64,
}

impl Instant {
    /// The zero instant (useful as a baseline in tests).
    pub const fn zero() -> Self {
        Self { nanos: 0 }
    }

    /// Construct an instant from an arbitrary nanosecond counter.
    pub fn from_nanos(nanos: u64) -> Self {
        Self { nanos }
    }

    /// Construct an instant from seconds.
    pub fn from_secs(secs: u32) -> Self {
        Self {
            nanos: (secs as u64) * 1_000_000_000,
        }
    }

    /// Add a duration, saturating on overflow.
    pub fn saturating_add(self, rhs: Duration) -> Self {
        Self {
            nanos: self.nanos.saturating_add(rhs.nanos),
        }
    }

    /// Compute `self - rhs` as a duration, returning `None` if `rhs` is later than `self`.
    pub fn checked_sub(self, rhs: Instant) -> Option<Duration> {
        if self.nanos >= rhs.nanos {
            Some(Duration::from_nanos(self.nanos - rhs.nanos))
        } else {
            None
        }
    }
}

/// A monotonic duration, stored as an unsigned nanosecond count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Duration {
    nanos: u64,
}

impl Duration {
    /// Construct a duration from seconds.
    pub const fn from_secs(secs: u32) -> Self {
        Self {
            nanos: secs as u64 * 1_000_000_000,
        }
    }

    /// Construct a duration from milliseconds.
    pub fn from_millis(millis: u32) -> Self {
        Self {
            nanos: millis as u64 * 1_000_000,
        }
    }

    /// Construct a duration from raw nanoseconds.
    pub const fn from_nanos(nanos: u64) -> Self {
        Self { nanos }
    }

    /// Multiply this duration by `rhs`, saturating on overflow.
    pub fn saturating_mul(self, rhs: u64) -> Self {
        Self {
            nanos: self.nanos.saturating_mul(rhs),
        }
    }

    /// Return this duration as a raw nanosecond count.
    pub fn as_u64_nanos(&self) -> u64 {
        self.nanos
    }
}

/// IEEE 1588 message-encoded log2 interval.
///
/// Many PTP fields use an `i8` log2 representation of seconds (e.g. `logMessageInterval`).
/// IEEE 1588 also defines a sentinel value `0x7F` meaning “not specified”.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogMessageInterval(i8);

impl LogMessageInterval {
    /// Construct a log-message-interval wrapper around a raw `i8`.
    pub const fn new(value: i8) -> Self {
        Self(value)
    }

    /// Construct the IEEE 1588 “not specified” value (`0x7F`).
    pub const fn unspecified() -> Self {
        // IEEE 1588 defines 0x7F as "not specified"
        Self(0x7F)
    }

    /// Convert into a [`LogInterval`] if the value is within the supported range.
    pub(crate) fn log_interval(&self) -> Option<LogInterval> {
        if self.0 >= LogInterval::MIN_LOG_VALUE && self.0 <= LogInterval::MAX_LOG_VALUE {
            Some(LogInterval::new(self.0))
        } else {
            None
        }
    }

    /// Return the raw on-the-wire representation.
    pub(crate) fn as_u8(self) -> u8 {
        self.0 as u8
    }
}

/// IEEE 1588 log2 interval used for scheduling and dataset fields.
///
/// The value `e` represents an interval of `2^e` seconds. Negative values represent fractions of
/// a second (e.g. `-1` is 0.5s).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogInterval {
    log_value: i8,
}

impl LogInterval {
    const MIN_LOG_VALUE: i8 = -20;
    const MAX_LOG_VALUE: i8 = 20;

    /// Construct a log2 interval, panicking if `log_value` is out of the supported range.
    ///
    /// This panic is part of `rptp`'s current fail-fast approach: `LogInterval` is a domain
    /// object and values outside its invariants are considered unrecoverable (they cannot
    /// participate meaningfully in the domain).
    pub const fn new(log_value: i8) -> Self {
        assert!(log_value >= Self::MIN_LOG_VALUE && log_value <= Self::MAX_LOG_VALUE);

        Self { log_value }
    }

    /// Convert this log2 interval into a concrete [`Duration`].
    pub fn duration(self) -> Duration {
        let e = self
            .log_value
            .clamp(Self::MIN_LOG_VALUE, Self::MAX_LOG_VALUE);

        if e >= 0 {
            let secs = 1u64 << e;
            let nanos = secs.saturating_mul(1_000_000_000);
            Duration::from_nanos(nanos)
        } else {
            let div = 1u64 << (-e);
            let nanos = 1_000_000_000 / div;
            Duration::from_nanos(nanos)
        }
    }

    pub(crate) fn log_message_interval(&self) -> LogMessageInterval {
        LogMessageInterval::new(self.log_value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic]
    fn timestamp_new_panics_on_invalid_seconds() {
        let _ = TimeStamp::new(1 << 48, 500_000_000);
    }

    #[test]
    #[should_panic]
    fn timestamp_new_panics_on_invalid_nanos() {
        let _ = TimeStamp::new(1, 1_000_000_000);
    }

    #[test]
    fn timestamp_subtraction() {
        let ts1 = TimeStamp::new(1, 500_000_000);
        let ts2 = TimeStamp::new(1, 200_000_000);

        let duration = ts1 - ts2;
        assert_eq!(duration, TimeInterval::new(0, 300_000_000));

        let duration = ts2 - ts1;
        assert_eq!(duration, TimeInterval::new(-1, 700_000_000));
    }

    #[test]
    fn timestamp_subtraction_max_range() {
        let ts1 = TimeStamp::new((1 << 48) - 1, 999_999_999);
        let ts2 = TimeStamp::new(0, 0);

        let duration = ts1 - ts2;
        assert_eq!(duration, TimeInterval::new((1 << 48) - 1, 999_999_999));

        let duration = ts2 - ts1;
        assert_eq!(duration, TimeInterval::new(-((1 << 48) - 1) - 1, 1));
    }

    #[test]
    fn timestamp_subtraction_crossing_seconds() {
        let ts1 = TimeStamp::new(2, 100_000_000);
        let ts2 = TimeStamp::new(1, 900_000_000);

        let duration = ts1 - ts2;
        assert_eq!(duration, TimeInterval::new(0, 200_000_000));

        let duration = ts2 - ts1;
        assert_eq!(duration, TimeInterval::new(-1, 800_000_000));
    }

    #[test]
    fn timestamp_subtraction_zero_duration() {
        let ts = TimeStamp::new(1, 500_000_000);
        let duration = ts - ts;
        assert_eq!(duration, TimeInterval::new(0, 0));
    }

    #[test]
    fn timestamp_checked_add_simple() {
        let ts = TimeStamp::new(1, 500_000_000);
        let interval = TimeInterval::new(2, 250_000_000);

        let result = ts.checked_add(interval).unwrap();
        assert_eq!(result, TimeStamp::new(3, 750_000_000));
    }

    #[test]
    fn timestamp_checked_add_with_nanos_carry() {
        let ts = TimeStamp::new(1, 900_000_000);
        let interval = TimeInterval::new(0, 200_000_000);

        let result = ts.checked_add(interval).unwrap();
        assert_eq!(result, TimeStamp::new(2, 100_000_000));
    }

    #[test]
    fn timestamp_checked_add_overflows_domain_range() {
        let ts = TimeStamp::new(0, 0);
        let interval = TimeInterval::new(i64::MAX, 0);

        let result = ts.checked_add(interval);
        assert!(result.is_none());
    }

    #[test]
    fn timestamp_checked_add_hits_upper_bound() {
        let ts = TimeStamp::new((1 << 48) - 2, 999_999_999);
        let interval = TimeInterval::new(0, 1);

        let result = ts.checked_add(interval).unwrap();
        assert_eq!(result, TimeStamp::new((1 << 48) - 1, 0));
    }

    #[test]
    fn timestamp_checked_add_beyond_upper_bound_returns_none() {
        let ts = TimeStamp::new((1 << 48) - 2, 0);
        let interval = TimeInterval::new(3, 0);

        let result = ts.checked_add(interval);
        assert!(result.is_none());
    }

    #[test]
    fn timestamp_checked_add_negative_interval_below_zero_returns_none() {
        let ts = TimeStamp::new(1, 0);
        let interval = TimeInterval::new(-2, 0);

        let result = ts.checked_add(interval);
        assert!(result.is_none());
    }

    #[test]
    fn timestamp_checked_sub_simple() {
        let ts = TimeStamp::new(3, 750_000_000);
        let interval = TimeInterval::new(2, 250_000_000);

        let result = ts.checked_sub(interval).unwrap();
        assert_eq!(result, TimeStamp::new(1, 500_000_000));
    }

    #[test]
    fn timestamp_checked_sub_with_nanos_borrow() {
        let ts = TimeStamp::new(2, 100_000_000);
        let interval = TimeInterval::new(0, 200_000_000);

        let result = ts.checked_sub(interval).unwrap();
        assert_eq!(result, TimeStamp::new(1, 900_000_000));
    }

    #[test]
    fn timestamp_checked_sub_below_zero_returns_none() {
        let ts = TimeStamp::new(1, 0);
        let interval = TimeInterval::new(2, 0);

        let result = ts.checked_sub(interval);
        assert!(result.is_none());
    }

    #[test]
    fn timestamp_checked_sub_negative_interval_overflows_upper_bound_returns_none() {
        let ts = TimeStamp::new((1 << 48) - 1, 0);
        let interval = TimeInterval::new(-1, 0);

        let result = ts.checked_sub(interval);
        assert!(result.is_none());
    }

    #[test]
    #[should_panic]
    fn time_interval_new_with_i64_min_and_zero_nanos_should_panic() {
        let _ = TimeInterval::new(i64::MIN, 0);
    }

    #[test]
    fn time_interval_half_zero() {
        let duration = TimeInterval::new(0, 0);
        assert_eq!(duration.half(), TimeInterval::new(0, 0));
    }

    #[test]
    fn time_interval_half_even_positive() {
        let duration = TimeInterval::new(2, 0);
        assert_eq!(duration.half(), TimeInterval::new(1, 0));
    }

    #[test]
    fn time_interval_half_positive_odd_rounds_towards_zero() {
        let duration = TimeInterval::new(0, 1);
        assert_eq!(duration.half(), TimeInterval::new(0, 0));
    }

    #[test]
    fn time_interval_half_negative_odd_rounds_towards_zero() {
        let duration = TimeInterval::new(-1, 999_999_999);
        assert_eq!(duration.half(), TimeInterval::new(0, 0));
    }

    #[test]
    fn time_interval_abs_positive() {
        let duration = TimeInterval::new(1, 500_000_000);
        assert_eq!(duration.abs(), TimeInterval::new(1, 500_000_000));
    }

    #[test]
    fn time_interval_abs_negative() {
        let duration = TimeInterval::new(-1, 500_000_000);
        assert_eq!(duration.abs(), TimeInterval::new(0, 500_000_000));
    }

    #[test]
    fn time_interval_abs_zero() {
        let duration = TimeInterval::new(0, 0);
        assert_eq!(duration.abs(), TimeInterval::new(0, 0));
    }

    #[test]
    fn time_interval_abs_zero_nanos() {
        let duration = TimeInterval::new(-1, 0);
        assert_eq!(duration.abs(), TimeInterval::new(1, 0));
    }

    #[test]
    fn time_interval_abs_max_nanos() {
        let duration = TimeInterval::new(-1, 999_999_999);
        assert_eq!(duration.abs(), TimeInterval::new(0, 1));
    }

    #[test]
    fn time_interval_abs_i64_min() {
        let duration = TimeInterval::new(i64::MIN, 1);
        let abs_duration = duration.abs();
        assert_eq!(abs_duration, TimeInterval::new(i64::MAX, 999_999_999));
    }

    #[test]
    fn time_interval_ord() {
        assert!(TimeInterval::new(-2, 0) < TimeInterval::new(-1, 999_999_999));

        // -0.999999999 < -0.1
        assert!(TimeInterval::new(-1, 1) < TimeInterval::new(-1, 999_999_999));
    }

    #[test]
    fn log_interval_duration() {
        let li = LogInterval::new(0);
        assert_eq!(li.duration(), Duration::from_secs(1));

        let li = LogInterval::new(1);
        assert_eq!(li.duration(), Duration::from_secs(2));

        let li = LogInterval::new(2);
        assert_eq!(li.duration(), Duration::from_secs(4));

        let li = LogInterval::new(-1);
        assert_eq!(li.duration(), Duration::from_millis(500));

        let li = LogInterval::new(-2);
        assert_eq!(li.duration(), Duration::from_millis(250));

        let li = LogInterval::new(-3);
        assert_eq!(li.duration(), Duration::from_millis(125));
    }

    #[test]
    #[should_panic]
    fn log_interval_new_panics_on_out_of_range_positive() {
        let _ = LogInterval::new(21);
    }

    #[test]
    #[should_panic]
    fn log_interval_new_panics_on_out_of_range_negative() {
        let _ = LogInterval::new(-21);
    }
}
