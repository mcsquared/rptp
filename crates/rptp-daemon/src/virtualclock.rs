use std::sync::Mutex;
use std::time::Instant as StdInstant;

use rptp::{
    clock::{Clock, SynchronizableClock, TimeScale},
    time::{TimeInterval, TimeStamp},
};

pub struct VirtualClock {
    start: Mutex<StdInstant>,
    ts: Mutex<TimeStamp>,
    rate: Mutex<f64>,
    time_scale: TimeScale,
}

impl VirtualClock {
    pub fn new(start_ts: TimeStamp, rate: f64, time_scale: TimeScale) -> Self {
        Self {
            start: Mutex::new(StdInstant::now()),
            ts: Mutex::new(start_ts),
            rate: Mutex::new(rate),
            time_scale,
        }
    }
}

impl Clock for VirtualClock {
    fn now(&self) -> TimeStamp {
        let start = self.start.lock().unwrap();
        let rate = *self.rate.lock().unwrap();
        let base = *self.ts.lock().unwrap();

        let dt = start.elapsed();
        let dt_nanos = dt.as_nanos() as f64 * rate;
        let dt_secs = (dt_nanos / 1_000_000_000.0) as i64;
        let dt_rem_nanos = (dt_nanos % 1_000_000_000.0) as u32;

        base.checked_add(TimeInterval::new(dt_secs, dt_rem_nanos))
            .unwrap_or(base)
    }

    fn time_scale(&self) -> TimeScale {
        self.time_scale
    }
}

impl Clock for &VirtualClock {
    fn now(&self) -> TimeStamp {
        (*self).now()
    }

    fn time_scale(&self) -> TimeScale {
        (*self).time_scale()
    }
}

impl SynchronizableClock for VirtualClock {
    fn step(&self, to: TimeStamp) {
        *self.start.lock().unwrap() = StdInstant::now();
        *self.ts.lock().unwrap() = to;
    }

    fn adjust(&self, rate: f64) {
        let current = self.now();
        *self.start.lock().unwrap() = StdInstant::now();
        *self.ts.lock().unwrap() = current;
        *self.rate.lock().unwrap() = rate;
    }
}

impl SynchronizableClock for &VirtualClock {
    fn step(&self, to: TimeStamp) {
        (*self).step(to)
    }

    fn adjust(&self, rate: f64) {
        (*self).adjust(rate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_clock_is_monotonic_for_fixed_rate() {
        let clock = VirtualClock::new(TimeStamp::new(1, 0), 1.0, TimeScale::Arb);

        let t1 = clock.now();
        let t2 = clock.now();

        assert!(t2 >= t1);
    }

    #[test]
    fn virtual_clock_does_not_go_backwards_on_adjust() {
        let clock = VirtualClock::new(TimeStamp::new(1, 0), 1.0, TimeScale::Arb);

        let t1 = clock.now();
        clock.adjust(0.5);
        let t2 = clock.now();

        assert!(t2 >= t1);
    }

    #[test]
    fn virtual_clock_step_sets_lower_bound() {
        let clock = VirtualClock::new(TimeStamp::new(0, 0), 1.0, TimeScale::Arb);

        clock.step(TimeStamp::new(5, 0));
        let t = clock.now();

        assert!(t >= TimeStamp::new(5, 0));
    }
}
