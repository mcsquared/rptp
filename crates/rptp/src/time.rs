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

    pub fn from_secs_f64(secs_f64: f64) -> Self {
        // Round to nearest nanosecond
        let total_nanos = (secs_f64 * 1_000_000_000.0).round();

        // Split total nanoseconds into integral seconds and nanoseconds
        let mut seconds = (total_nanos / 1_000_000_000.0).trunc() as i64;
        let mut nanos = (total_nanos % 1_000_000_000.0).abs() as u32;

        // Normalize negative durations:
        // If secs < 0 and nanos > 0, "borrow" one second to make nanos positive
        if total_nanos < 0.0 && nanos > 0 {
            seconds -= 1;
            nanos = 1_000_000_000 - nanos;
        }

        Self { seconds, nanos }
    }

    pub fn as_secs_f64(&self) -> f64 {
        self.seconds as f64 + (self.nanos as f64 / 1_000_000_000 as f64)
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
}
