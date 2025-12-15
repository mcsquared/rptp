#![no_std]
#![no_main]

use core::cell::{Cell, UnsafeCell};
use core::sync::atomic::{AtomicU32, Ordering};

use cortex_m::peripheral::SYST;
use cortex_m::peripheral::syst::SystClkSource;
use cortex_m_rt::{entry, exception};
use cortex_m_semihosting::hprintln;
use heapless::{Deque, Vec};
use panic_halt as _;
use rptp::{
    bmca::{DefaultDS, IncrementalBmca, Priority1, Priority2},
    clock::{Clock, ClockIdentity, ClockQuality, LocalClock, StepsRemoved, SynchronizableClock},
    heapless::HeaplessSortedForeignClockRecords,
    log::{NOOP_CLOCK_METRICS, PortEvent, PortLog},
    message::{EventMessage, MessageIngress, SystemMessage, TimeScale, TimestampMessage},
    port::{
        DomainNumber, DomainPort, PhysicalPort, PortMap, PortNumber, SendResult,
        SingleDomainPortMap, Timeout, TimerHost,
    },
    portstate::PortProfile,
    result::Error as RptpError,
    servo::{Servo, SteppingServo},
    time::{Duration, Instant, TimeInterval, TimeStamp},
    timestamping::TxTimestamping,
};
use smoltcp::time::Instant as SmolInstant;
use smoltcp::wire::Ipv4Address;

mod net;

use net::Lm3sEth;
use net::{PtpNetwork, PtpStorage, RxKind, RxPacket};

struct StorageCell(UnsafeCell<PtpStorage>);
struct TimestampQueue(UnsafeCell<Deque<SystemMessage, 8>>);

static PTP_NETWORK_STORAGE: StorageCell = StorageCell(UnsafeCell::new(PtpStorage::new()));
static TIMESTAMP_QUEUE: TimestampQueue = TimestampQueue(UnsafeCell::new(Deque::new()));

unsafe impl Sync for StorageCell {}
unsafe impl Sync for TimestampQueue {}

impl TimestampQueue {
    fn push(&self, msg: SystemMessage) {
        unsafe {
            let result = (&mut *self.0.get()).push_back(msg);
            if result.is_err() {
                panic!("[timestamp queue] overflow; dropping timestamp message");
            }
        }
    }

    fn pop(&self) -> Option<SystemMessage> {
        unsafe { (&mut *self.0.get()).pop_front() }
    }
}

#[derive(Clone, Copy)]
struct Ticks {
    ticks: u32,
}

impl Ticks {
    const NANOS_PER_TICK: u32 = 10_000_000; // 1 tick is defined as 10ms

    fn new(ticks: u32) -> Self {
        Self { ticks }
    }

    fn elapsed_since(&self, since: &Ticks) -> Ticks {
        Ticks::new(self.ticks.wrapping_sub(since.ticks))
    }

    fn as_duration(&self) -> Duration {
        let nanos = self.ticks as u64 * Self::NANOS_PER_TICK as u64;
        Duration::from_nanos(nanos)
    }

    // Note: with a u32 tick counter and 10ms per tick, this should be safe for ~497 days.
    // More than a enough for this simple demo.
    fn as_smol_instant(&self) -> SmolInstant {
        SmolInstant::from_micros(self.ticks * 10_000)
    }
}

struct SystemTick {
    ticks: AtomicU32,
}

impl SystemTick {
    const fn new() -> Self {
        Self {
            ticks: AtomicU32::new(0),
        }
    }

    fn init(&self, syst: &mut SYST) {
        syst.set_clock_source(SystClkSource::Core);
        // hard coded assumption of a 12MHz clock, so 120_0000 cycles
        // should let the interupt fire at a 10ms rate.
        syst.set_reload(120_000 - 1);
        syst.clear_current();
        syst.enable_interrupt();
        syst.enable_counter();
    }

    fn tick(&self) {
        self.ticks.fetch_add(1, Ordering::Relaxed);
    }

    fn now(&self) -> Ticks {
        Ticks::new(self.ticks.load(Ordering::Relaxed))
    }
}

static SYSTEM_TICK: SystemTick = SystemTick::new();

struct InstantClock {
    system_tick: &'static SystemTick,
    last_ticks: Cell<Ticks>,
    instant: Cell<Instant>,
}

impl InstantClock {
    fn new(system_tick: &'static SystemTick, now: Ticks) -> Self {
        Self {
            system_tick,
            last_ticks: Cell::new(now),
            instant: Cell::new(Instant::from_nanos(0)),
        }
    }

    fn now(&self) -> Instant {
        let now_ticks = self.system_tick.now();
        let ticks_elapsed = now_ticks.elapsed_since(&self.last_ticks.get());
        self.last_ticks.set(now_ticks);

        let updated = self
            .instant
            .get()
            .saturating_add(ticks_elapsed.as_duration());
        self.instant.set(updated);
        updated
    }
}

#[exception]
fn SysTick() {
    SYSTEM_TICK.tick();
}

struct SemiHostingTime;

impl SemiHostingTime {
    const TAI_OFFSET_SECONDS: u64 = 37;

    fn now() -> Option<TimeStamp> {
        let secs = unsafe { cortex_m_semihosting::syscall!(TIME, 0) } as i64;
        if secs < 0 {
            return None;
        }

        let clock_ticks = unsafe { cortex_m_semihosting::syscall!(CLOCK) } as i64;
        let tickfreq = unsafe { cortex_m_semihosting::syscall!(TICKFREQ) } as i64;
        if clock_ticks < 0 || tickfreq <= 0 {
            return None;
        }

        let total_nanos = clock_ticks.saturating_mul(1_000_000_000i64) / tickfreq;
        let extra_secs = total_nanos.div_euclid(1_000_000_000);
        let nanos = total_nanos.rem_euclid(1_000_000_000) as u32;
        // PTP uses the TAI time scale; add the current TAI-UTC offset so the grandmaster
        // epoch aligns with ptp4l expectations.
        let secs = (secs + extra_secs + Self::TAI_OFFSET_SECONDS as i64) as u64;

        Some(TimeStamp::new(secs, nanos))
    }
}

#[derive(Clone, Copy)]
struct Cycles {
    cycles: u32,
}

impl Cycles {
    const CPU_HZ: u32 = 12_000_000;

    fn now() -> Self {
        let dwt = unsafe { &*cortex_m::peripheral::DWT::PTR };
        let cyccnt = dwt.cyccnt.read();
        Self { cycles: cyccnt }
    }

    fn elapsed_since(&self, since: Cycles) -> TimeInterval {
        let delta_cycles = self.cycles.wrapping_sub(since.cycles) as u64;
        let nanos = delta_cycles.saturating_mul(1_000_000_000u64) / (Self::CPU_HZ as u64);
        TimeInterval::from_u64_nanos(nanos)
    }
}

struct CycleClock {
    time: Cell<TimeStamp>,
    last_cycles: Cell<Cycles>,
}

impl CycleClock {
    fn new(time: TimeStamp, now: Cycles) -> Self {
        Self {
            time: Cell::new(time),
            last_cycles: Cell::new(now),
        }
    }
}

impl Clock for CycleClock {
    fn now(&self) -> TimeStamp {
        let cur_cycles = Cycles::now();
        let delta = cur_cycles.elapsed_since(self.last_cycles.get());
        self.last_cycles.set(cur_cycles);
        self.time.set(
            self.time
                .get()
                .checked_add(delta)
                .unwrap_or(self.time.get()),
        );
        self.time.get()
    }
}

impl Clock for &CycleClock {
    fn now(&self) -> TimeStamp {
        (*self).now()
    }
}

impl SynchronizableClock for &CycleClock {
    fn step(&self, _to: TimeStamp) {
        // no-op for demo; master does not need to step
    }

    fn adjust(&self, _rate: f64) {
        // no-op for demo; master does not need to adjust
    }
}

struct DemoPhysicalPort {
    inner: net::SmoltcpPhysicalPort,
}

impl PhysicalPort for DemoPhysicalPort {
    fn send_event(&self, buf: &[u8]) -> SendResult {
        self.inner.send_event(buf)
    }

    fn send_general(&self, buf: &[u8]) -> SendResult {
        self.inner.send_general(buf)
    }
}

struct DemoTimeout<'a> {
    slot: usize,
    instant_clock: &'a InstantClock,
}

#[derive(Clone, Copy)]
struct TimerSlot {
    active: bool,
    deadline: Instant,
    msg: SystemMessage,
}

const MAX_TIMERS: usize = 16;

struct TimerStorage {
    slots: UnsafeCell<[TimerSlot; MAX_TIMERS]>,
}

unsafe impl Sync for TimerStorage {}

static TIMER_SLOTS: TimerStorage = TimerStorage {
    slots: UnsafeCell::new(
        [TimerSlot {
            active: false,
            deadline: Instant::zero(),
            msg: SystemMessage::Initialized,
        }; MAX_TIMERS],
    ),
};

impl TimerSlot {
    fn alloc<'a>(
        instant_clock: &'a InstantClock,
        msg: SystemMessage,
        delay: Duration,
    ) -> Option<DemoTimeout<'a>> {
        let now = instant_clock.now();
        let deadline = now.saturating_add(delay);

        // Safety: single-core, main loop only mutates slots through this API.
        unsafe {
            let slots = &mut *TIMER_SLOTS.slots.get();
            for (idx, slot) in slots.iter_mut().enumerate() {
                if !slot.active {
                    slot.active = true;
                    slot.deadline = deadline;
                    slot.msg = msg;
                    hprintln!("[timer] alloc slot={} msg={:?} delay={:?}", idx, msg, delay);
                    return Some(DemoTimeout {
                        slot: idx,
                        instant_clock,
                    });
                }
            }
        }

        None
    }

    fn restart(&mut self, instant_clock: &InstantClock, delay: Duration) {
        let now = instant_clock.now();
        self.deadline = now.saturating_add(delay);
        self.active = true;
    }
}

struct DemoTimerHost<'a> {
    instant_clock: &'a InstantClock,
}

impl<'a> DemoTimerHost<'a> {
    fn new(instant_clock: &'a InstantClock) -> Self {
        Self { instant_clock }
    }
}

impl<'a> TimerHost for DemoTimerHost<'a> {
    type Timeout = DemoTimeout<'a>;

    fn timeout(&self, msg: SystemMessage, delay: Duration) -> Self::Timeout {
        hprintln!("[timerhost] schedule msg={:?} delay={:?}", msg, delay);
        TimerSlot::alloc(self.instant_clock, msg, delay).unwrap_or_else(|| {
            hprintln!("[timerhost] no free timer slots for {:?}", msg);
            panic!("TimerSlot exhausted; increase MAX_TIMERS");
        })
    }
}

impl<'a> Timeout for DemoTimeout<'a> {
    fn restart(&self, timeout: Duration) {
        // Safety: single-core, slots only mutated here and in main loop.
        unsafe {
            let slots = &mut *TIMER_SLOTS.slots.get();
            if let Some(slot) = slots.get_mut(self.slot) {
                slot.restart(self.instant_clock, timeout);
            }
        }
    }
}

impl<'a> Drop for DemoTimeout<'a> {
    fn drop(&mut self) {
        // No-op: lifetime is managed by the timer slot; dropping the handle
        // must not clear the active flag prematurely.
    }
}

struct DemoTimestamping<'a> {
    clock: &'a CycleClock,
}

impl<'a> DemoTimestamping<'a> {
    fn new(clock: &'a CycleClock) -> Self {
        Self { clock }
    }
}

impl<'a> TxTimestamping for DemoTimestamping<'a> {
    fn stamp_egress(&self, msg: EventMessage) {
        let ts = self.clock.now();
        TIMESTAMP_QUEUE.push(SystemMessage::Timestamp(TimestampMessage::new(msg, ts)));
    }
}

struct DemoPortLog;

impl PortLog for DemoPortLog {
    fn message_sent(&self, msg: &str) {
        hprintln!("[sent] {}", msg);
    }

    fn message_received(&self, msg: &str) {
        hprintln!("[recv] {}", msg);
    }

    fn port_event(&self, event: PortEvent) {
        match event {
            PortEvent::Initialized => {
                hprintln!("[event] Initialized");
            }
            PortEvent::RecommendedSlave { parent } => {
                hprintln!("[event] RecommendedSlave parent={}", parent);
            }
            PortEvent::RecommendedMaster => {
                hprintln!("[event] RecommendedMaster");
            }
            PortEvent::MasterClockSelected { parent } => {
                hprintln!("[event] MasterClockSelected parent={}", parent);
            }
            PortEvent::AnnounceReceiptTimeout => {
                hprintln!("[event] AnnounceReceiptTimeout");
            }
            PortEvent::QualifiedMaster => {
                hprintln!("[event] QualifiedMaster");
            }
            PortEvent::SynchronizationFault => {
                hprintln!("[event] SynchronizationFault");
            }
            PortEvent::Static(msg) => {
                hprintln!("[event] {}", msg);
            }
        }
    }
}

fn demo_default_ds() -> DefaultDS {
    // Use a deterministic, but arbitrary, local clock identity and quality.
    DefaultDS::new(
        ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
        Priority1::new(100),
        Priority2::new(127),
        ClockQuality::new(100, 0xFE, 0xFFFF),
        TimeScale::Ptp,
    )
}

type DemoDomainPort<'a> =
    DomainPort<'a, &'a CycleClock, DemoPhysicalPort, DemoTimerHost<'a>, DemoTimestamping<'a>>;
type DemoBmca = IncrementalBmca<HeaplessSortedForeignClockRecords<4>>;
type DemoPortMap<'a> = SingleDomainPortMap<DemoDomainPort<'a>, DemoBmca, DemoPortLog>;

#[entry]
fn main() -> ! {
    let cp = cortex_m::Peripherals::take().unwrap();
    let mut syst = cp.SYST;
    let mut dcb = cp.DCB;
    let mut dwt = cp.DWT;

    // Enable the DWT cycle counter to get sub-microsecond timestamps.
    dcb.enable_trace();
    dwt.enable_cycle_counter();
    // Safety: cycle counter is part of the core DWT; we only reset it once at startup.
    unsafe { dwt.cyccnt.write(0) };

    SYSTEM_TICK.init(&mut syst);
    let instant_clock = InstantClock::new(&SYSTEM_TICK, SYSTEM_TICK.now());

    // Align the demo clock to host wall time via semihosting so QEMU exposes a
    // realistic grandmaster epoch. Fall back to a zero epoch if the host time
    // is unavailable.
    let seed_ts = SemiHostingTime::now().unwrap_or_else(|| TimeStamp::new(0, 0));
    if seed_ts == TimeStamp::new(0, 0) {
        hprintln!("warning: host time unavailable; using zero epoch");
    }
    let demo_clock = CycleClock::new(seed_ts, Cycles::now());

    hprintln!("rptp embedded demo starting (initializing)");

    let mac = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
    let ip = Ipv4Address::new(192, 168, 7, 2);
    let storage = unsafe { &mut *PTP_NETWORK_STORAGE.0.get() };
    let mut device = Lm3sEth::new();

    device.init(mac);

    let mut network = PtpNetwork::new(
        device,
        storage,
        mac,
        ip,
        SYSTEM_TICK.now().as_smol_instant(),
    );

    let local_clock = LocalClock::new(
        &demo_clock,
        demo_default_ds(),
        StepsRemoved::new(0),
        Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
    );

    let port = DomainPort::new(
        &local_clock,
        DemoPhysicalPort {
            inner: network.physical_port(),
        },
        DemoTimerHost::new(&instant_clock),
        DemoTimestamping::new(&demo_clock),
        DomainNumber::new(0),
        PortNumber::new(1),
    );

    let bmca = IncrementalBmca::new(HeaplessSortedForeignClockRecords::<4>::new());

    let state = PortProfile::default().initializing(port, bmca, DemoPortLog);
    let mut port_map: DemoPortMap = SingleDomainPortMap::new(DomainNumber::new(0), state);
    port_map
        .port_by_domain(DomainNumber::new(0))
        .map(|port| port.process_system_message(SystemMessage::Initialized))
        .ok();

    let mut rx_packets: Vec<RxPacket, { net::MAX_RX_PACKETS }> = Vec::new();
    loop {
        let now = instant_clock.now();
        let smoltcp_now = SYSTEM_TICK.now().as_smol_instant();
        network.poll(smoltcp_now, &mut rx_packets);

        for packet in rx_packets.iter() {
            process_packet(&mut port_map, packet, now, demo_clock.now());
        }
        rx_packets.clear();

        // Deliver any TX timestamps emitted by the port.
        while let Some(msg) = TIMESTAMP_QUEUE.pop() {
            port_map
                .port_by_domain(DomainNumber::new(0))
                .map(|port| port.process_system_message(msg))
                .ok();
        }

        // Drive timers: for any expired timer, inject the corresponding
        // SystemMessage into the port state machine.
        unsafe {
            let slots = &mut *TIMER_SLOTS.slots.get();
            for slot in slots.iter_mut() {
                if slot.active && now >= slot.deadline {
                    slot.active = false;
                    hprintln!("[timer] fire slot msg={:?}", slot.msg);
                    port_map
                        .port_by_domain(DomainNumber::new(0))
                        .map(|port| port.process_system_message(slot.msg))
                        .ok();
                }
            }
        }

        cortex_m::asm::wfi();
    }
}

fn process_packet<'a>(
    port_map: &mut DemoPortMap<'a>,
    packet: &RxPacket,
    now: Instant,
    ingress_timestamp: TimeStamp,
) {
    let result = match packet.kind {
        RxKind::Event => MessageIngress::new(port_map)
            .receive_event(&packet.data[..packet.len], ingress_timestamp),
        RxKind::General => {
            MessageIngress::new(port_map).receive_general(&packet.data[..packet.len], now)
        }
    };

    if let Err(e) = result {
        match e {
            RptpError::Parse(_) => hprintln!("[rx] drop malformed packet: {:?}", e),
            _ => hprintln!("[rx] drop protocol error: {:?}", e),
        }
    }
}
