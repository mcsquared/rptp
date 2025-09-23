use crate::time::TimeStamp;

pub trait Clock: Send {
    fn now(&self) -> TimeStamp;
}

pub struct FakeClock {
    now: TimeStamp,
}

impl FakeClock {
    pub fn new(now: TimeStamp) -> Self {
        Self { now }
    }
}

impl Clock for FakeClock {
    fn now(&self) -> TimeStamp {
        self.now
    }
}
