#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeStamp {
    seconds: u64,
    nanos: u32,
}

impl TimeStamp {
    pub fn new(seconds: u64, nanos: u32) -> Self {
        assert!(seconds < (1 << 47));
        assert!(nanos < 1_000_000_000);
        Self { seconds, nanos }
    }

    pub fn to_wire(&self) -> [u8; 10] {
        let mut buf = [0; 10];
        buf[0..2].copy_from_slice(&((self.seconds >> 32) as u16).to_be_bytes());
        buf[2..6].copy_from_slice(&(self.seconds as u32).to_be_bytes());
        buf[6..10].copy_from_slice(&self.nanos.to_be_bytes());
        buf
    }
}

impl std::ops::Sub<Duration> for TimeStamp {
    type Output = TimeStamp;

    fn sub(self, rhs: Duration) -> Self::Output {
        let mut seconds = self.seconds as i64 - rhs.seconds;
        let mut nanos = self.nanos as i32 - rhs.nanos as i32;

        if nanos < 0 {
            seconds -= 1;
            nanos += 1_000_000_000;
        }

        assert!(seconds >= 0);
        TimeStamp::new(seconds as u64, nanos as u32)
    }
}

impl std::ops::Sub for TimeStamp {
    type Output = Duration;

    fn sub(self, rhs: Self) -> Self::Output {
        let mut delta_seconds = self.seconds as i64 - rhs.seconds as i64;
        let mut delta_nanos = self.nanos as i32 - rhs.nanos as i32;

        if delta_nanos < 0 {
            delta_seconds -= 1;
            delta_nanos += 1_000_000_000;
        }

        Duration::new(delta_seconds, delta_nanos as u32)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Duration {
    seconds: i64,
    nanos: u32,
}

impl Duration {
    pub fn new(seconds: i64, nanos: u32) -> Self {
        Self { seconds, nanos }
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

    pub fn half(self) -> Self {
        Self::from_total_nanos(self.total_nanos() / 2)
    }
}

impl std::ops::Sub for Duration {
    type Output = Duration;

    fn sub(self, rhs: Self) -> Self::Output {
        let mut delta_seconds = self.seconds - rhs.seconds;
        let mut delta_nanos = self.nanos as i32 - rhs.nanos as i32;

        if delta_nanos < 0 {
            delta_seconds -= 1;
            delta_nanos += 1_000_000_000;
        }

        Duration::new(delta_seconds, delta_nanos as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic]
    fn timestamp_new_panics_on_invalid_seconds() {
        let _ = TimeStamp::new(1 << 47, 500_000_000);
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
        assert_eq!(duration, Duration::new(0, 300_000_000));

        let duration = ts2 - ts1;
        assert_eq!(duration, Duration::new(-1, 700_000_000));
    }

    #[test]
    fn timestamp_subtraction_max_range() {
        let ts1 = TimeStamp::new((1 << 47) - 1, 999_999_999);
        let ts2 = TimeStamp::new(0, 0);

        let duration = ts1 - ts2;
        assert_eq!(duration, Duration::new((1 << 47) - 1, 999_999_999));

        let duration = ts2 - ts1;
        assert_eq!(duration, Duration::new(-((1 << 47) - 1) - 1, 1));
    }

    #[test]
    fn timestamp_subtraction_crossing_seconds() {
        let ts1 = TimeStamp::new(2, 100_000_000);
        let ts2 = TimeStamp::new(1, 900_000_000);

        let duration = ts1 - ts2;
        assert_eq!(duration, Duration::new(0, 200_000_000));

        let duration = ts2 - ts1;
        assert_eq!(duration, Duration::new(-1, 800_000_000));
    }

    #[test]
    fn timestamp_subtraction_zero_duration() {
        let ts = TimeStamp::new(1, 500_000_000);
        let duration = ts - ts;
        assert_eq!(duration, Duration::new(0, 0));
    }

    #[test]
    fn duration_half_zero() {
        let duration = Duration::new(0, 0);
        assert_eq!(duration.half(), Duration::new(0, 0));
    }

    #[test]
    fn duration_half_even_positive() {
        let duration = Duration::new(2, 0);
        assert_eq!(duration.half(), Duration::new(1, 0));
    }

    #[test]
    fn duration_half_positive_odd_rounds_towards_zero() {
        let duration = Duration::new(0, 1);
        assert_eq!(duration.half(), Duration::new(0, 0));
    }

    #[test]
    fn duration_half_negative_odd_rounds_towards_zero() {
        let duration = Duration::new(-1, 999_999_999);
        assert_eq!(duration.half(), Duration::new(0, 0));
    }
}
