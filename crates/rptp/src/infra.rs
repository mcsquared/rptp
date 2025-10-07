pub mod infra_support {
    use std::cell::RefCell;
    use std::cmp;

    use crate::bmca::{ForeignClock, SortedForeignClocks};

    pub struct SortedForeignClocksVec {
        clocks: RefCell<Vec<ForeignClock>>,
        cmp: fn(&ForeignClock, &ForeignClock) -> cmp::Ordering,
    }

    impl SortedForeignClocksVec {
        pub fn new(cmp: fn(&ForeignClock, &ForeignClock) -> cmp::Ordering) -> Self {
            Self {
                clocks: RefCell::new(Vec::new()),
                cmp,
            }
        }
    }

    impl SortedForeignClocks for SortedForeignClocksVec {
        fn insert(&self, clock: ForeignClock) {
            let mut clocks = self.clocks.borrow_mut();

            match clocks.binary_search_by(|probe| (self.cmp)(probe, &clock)) {
                Ok(idx) => clocks[idx] = clock,
                Err(idx) => clocks.insert(idx, clock),
            }
        }

        fn first(&self) -> Option<ForeignClock> {
            self.clocks.borrow().first().cloned()
        }
    }

    impl SortedForeignClocks for Box<SortedForeignClocksVec> {
        fn insert(&self, clock: ForeignClock) {
            self.as_ref().insert(clock);
        }

        fn first(&self) -> Option<ForeignClock> {
            self.as_ref().first()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_sorted_vec_foreign_clock_insert_single_item() {
            let sorted_clocks = SortedForeignClocksVec::new(|_a, _b| cmp::Ordering::Equal);
            let clock = ForeignClock::new();
            sorted_clocks.insert(clock);
            assert_eq!(sorted_clocks.first(), Some(clock));
        }

        #[test]
        fn test_sorted_vec_foreign_clock_insert_items_equal() {
            let sorted_clocks = SortedForeignClocksVec::new(|_a, _b| cmp::Ordering::Equal);
            let clock1 = ForeignClock::new();
            let clock2 = ForeignClock::new();
            sorted_clocks.insert(clock1);
            sorted_clocks.insert(clock2);
            assert_eq!(sorted_clocks.first(), Some(clock1));
        }

        #[test]
        fn test_sorted_vec_foreign_clock_insert_items_less() {
            let sorted_clocks = SortedForeignClocksVec::new(|_a, _b| cmp::Ordering::Less);
            let clock1 = ForeignClock::new();
            let clock2 = ForeignClock::new();
            sorted_clocks.insert(clock1);
            sorted_clocks.insert(clock2);
            assert_eq!(sorted_clocks.first(), Some(clock1));
        }

        #[test]
        fn test_sorted_vec_foreign_clock_insert_items_greater() {
            let sorted_clocks = SortedForeignClocksVec::new(|_a, _b| cmp::Ordering::Greater);
            let clock1 = ForeignClock::new();
            let clock2 = ForeignClock::new();
            sorted_clocks.insert(clock1);
            sorted_clocks.insert(clock2);
            assert_eq!(sorted_clocks.first(), Some(clock2));
        }
    }
}
