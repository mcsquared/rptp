// Explicit construction makes such invalid combinations impossible.
// fn ptp_clock_construction_explicit() {
//     let domain_0 = Domain::new(0);
//     let domain_1 = Domain::new(1);

//     let local_clock_0 = LocalClock::<dyn SynchronizableClock>::new(domain_0);
//     let another_local_clock_0 = LocalClock::<dyn SynchronizableClock>::new(domain_0);
//     let local_clock_1 = LocalClock::<dyn SynchronizableClock>::new(domain_1);

//     let physical_port_0 = Rc::new(PhysicalPort::new("eth0"));
//     let physical_port_1 = Rc::new(PhysicalPort::new("eth1"));

//     let ordinary_clock = OrdinaryClock::new(OrdinaryPort::new(
//         local_clock_0,
//         physical_port_0.clone(),
//         OrdinaryBmca::new(),
//     ));

//     // this should be illegal and should produce a runtime error if attempted
//     // its because there shall be only on port per domain per physical interface
//     // -> sort this would associate another port to eth0, that is attached to domain zero, because
//     // its clock is associated to domain zero
//     let ordinary_clock_illegal = OrdinaryClock::new(OrdinaryPort::new(
//         another_local_clock_0,
//         physical_port_0.clone(),
//         OrdinaryBmca::new(),
//     ));

//     let boundary_bmca = BoundaryBmca::new();
//     let boundary_clock = BoundaryClock::new(
//         BoundaryPort::new(
//             local_clock_1.clone(),
//             physical_port_0.clone(),
//             boundary_bmca.clone(),
//         ),
//         BoundaryPort::new(
//             local_clock_1.clone(),
//             physical_port_1.clone(),
//             boundary_bmca.clone(),
//         ),
//     );
// }

// struct OrdinaryClock<P: PhysicalPort> {
//     port: OrdinaryPort<P>,
// }
