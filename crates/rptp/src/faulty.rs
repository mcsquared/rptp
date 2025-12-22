use core::marker::PhantomData;

use crate::bmca::Bmca;
use crate::port::Port;

// FaultyPort stub implementation. At the moment it does nothing and is a dead end. For the current
// phase of development this is sufficient as we only need to model the existence of faulty ports.
// TODO: implement proper transistions into and out of the faulty state, passing actual port, bmca
// and log like a baton like the other port states do.
// TODO: implement BMCA reset and other faulty port behavior.
pub struct FaultyPort<P: Port, B: Bmca> {
    _port: PhantomData<P>,
    _bmca: PhantomData<B>,
}

impl<P: Port, B: Bmca> Default for FaultyPort<P, B> {
    fn default() -> Self {
        Self {
            _port: PhantomData,
            _bmca: PhantomData,
        }
    }
}
