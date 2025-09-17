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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Duration {
    _seconds: i64,
    _nanos: u32,
}

impl Duration {
    pub fn new(_seconds: i64, _nanos: u32) -> Self {
        Self { _seconds, _nanos }
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
}
