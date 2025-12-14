use std::cell::RefCell;
use std::time::Instant as StdInstant;

use rptp::{
    clock::{Clock, SynchronizableClock},
    time::{TimeInterval, TimeStamp},
};

pub struct VirtualClock {
    start: RefCell<StdInstant>,
    ts: RefCell<TimeStamp>,
    rate: RefCell<f64>,
}

impl VirtualClock {
    pub fn new(start_ts: TimeStamp, rate: f64) -> Self {
        Self {
            start: RefCell::new(StdInstant::now()),
            ts: RefCell::new(start_ts),
            rate: RefCell::new(rate),
        }
    }
}

impl Clock for VirtualClock {
    fn now(&self) -> TimeStamp {
        let dt = self.start.borrow().elapsed();
        let dt_nanos = dt.as_nanos() as f64 * *self.rate.borrow();
        let dt_secs = (dt_nanos / 1_000_000_000.0) as i64;
        let dt_rem_nanos = (dt_nanos % 1_000_000_000.0) as u32;

        let base = *self.ts.borrow();
        base.checked_add(TimeInterval::new(dt_secs, dt_rem_nanos))
            .unwrap_or(base)
    }
}

impl Clock for &VirtualClock {
    fn now(&self) -> TimeStamp {
        (*self).now()
    }
}

impl SynchronizableClock for VirtualClock {
    fn step(&self, to: TimeStamp) {
        self.start.replace(StdInstant::now());
        self.ts.replace(to);
    }

    fn adjust(&self, rate: f64) {
        let current = self.now();
        self.start.replace(StdInstant::now());
        self.ts.replace(current);
        self.rate.replace(rate);
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
        let clock = VirtualClock::new(TimeStamp::new(1, 0), 1.0);

        let t1 = clock.now();
        let t2 = clock.now();

        assert!(t2 >= t1);
    }

    #[test]
    fn virtual_clock_does_not_go_backwards_on_adjust() {
        let clock = VirtualClock::new(TimeStamp::new(1, 0), 1.0);

        let t1 = clock.now();
        clock.adjust(0.5);
        let t2 = clock.now();

        assert!(t2 >= t1);
    }

    #[test]
    fn virtual_clock_step_sets_lower_bound() {
        let clock = VirtualClock::new(TimeStamp::new(0, 0), 1.0);

        clock.step(TimeStamp::new(5, 0));
        let t = clock.now();

        assert!(t >= TimeStamp::new(5, 0));
    }
}
