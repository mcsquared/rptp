use core::cell::UnsafeCell;
use core::ptr;

use heapless::Vec;
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet, SocketStorage};
use smoltcp::phy::{self, ChecksumCapabilities, Device, DeviceCapabilities, Medium};
use smoltcp::socket::udp::{PacketBuffer, PacketMetadata, Socket as UdpSocket};
use smoltcp::time::Instant as SmolInstant;
use smoltcp::wire::{
    EthernetAddress, HardwareAddress, IpAddress, IpCidr, IpEndpoint, IpListenEndpoint, Ipv4Address,
    Ipv4Cidr,
};

use rptp::port::{PhysicalPort, SendError, SendResult};

pub const MAX_FRAME_SIZE: usize = 1500;
pub const MAX_RX_PACKETS: usize = 4;

const ENET_BASE: usize = 0x4004_8000;
const REG_RIS: usize = 0x00;
const REG_RCTL: usize = 0x08;
const REG_TCTL: usize = 0x0c;
const REG_DATA: usize = 0x10;
const REG_IA0: usize = 0x14;
const REG_IA1: usize = 0x18;
const REG_THR: usize = 0x1c;
const REG_NP: usize = 0x34;
const REG_TR: usize = 0x38;

const SE_RCTL_RXEN: u32 = 0x01;
const SE_RCTL_AMUL: u32 = 0x02;
const SE_RCTL_PRMS: u32 = 0x04;
const SE_RCTL_RSTFIFO: u32 = 0x10;

const SE_TCTL_TXEN: u32 = 0x01;
const SE_TCTL_PADEN: u32 = 0x02;
const SE_TCTL_CRC: u32 = 0x04;
const SE_TCTL_DUPLEX: u32 = 0x08;

#[derive(Clone, Copy)]
pub enum RxKind {
    Event,
    General,
}

pub struct RxPacket {
    pub len: usize,
    pub data: [u8; MAX_FRAME_SIZE],
    pub kind: RxKind,
}

pub struct PtpStorage {
    pub socket_storage: [SocketStorage<'static>; 2],
    pub event_rx_meta: [PacketMetadata; 4],
    pub event_tx_meta: [PacketMetadata; 4],
    pub event_rx_buf: [u8; MAX_FRAME_SIZE],
    pub event_tx_buf: [u8; MAX_FRAME_SIZE],
    pub general_rx_meta: [PacketMetadata; 4],
    pub general_tx_meta: [PacketMetadata; 4],
    pub general_rx_buf: [u8; MAX_FRAME_SIZE],
    pub general_tx_buf: [u8; MAX_FRAME_SIZE],
}

impl PtpStorage {
    pub const fn new() -> Self {
        Self {
            socket_storage: [SocketStorage::EMPTY; 2],
            event_rx_meta: [PacketMetadata::EMPTY; 4],
            event_tx_meta: [PacketMetadata::EMPTY; 4],
            event_rx_buf: [0; MAX_FRAME_SIZE],
            event_tx_buf: [0; MAX_FRAME_SIZE],
            general_rx_meta: [PacketMetadata::EMPTY; 4],
            general_tx_meta: [PacketMetadata::EMPTY; 4],
            general_rx_buf: [0; MAX_FRAME_SIZE],
            general_tx_buf: [0; MAX_FRAME_SIZE],
        }
    }
}

#[derive(Clone, Copy)]
struct Frame {
    len: usize,
    data: [u8; MAX_FRAME_SIZE],
}

impl Frame {
    fn new_with_len(len: usize) -> Self {
        Self {
            len,
            data: [0; MAX_FRAME_SIZE],
        }
    }
}

struct PtpSockets {
    set: SocketSet<'static>,
    event_handle: SocketHandle,
    general_handle: SocketHandle,
}

pub struct PtpNetwork<D: Device> {
    iface: Interface,
    device: D,
    sockets: UnsafeCell<PtpSockets>,
}

#[derive(Clone, Copy)]
pub struct SmoltcpPhysicalPort {
    sockets: *const UnsafeCell<PtpSockets>,
}

pub struct Lm3sEth {
    mac: [u8; 6],
}

unsafe impl Sync for Lm3sEth {}

impl Lm3sEth {
    pub const fn new() -> Self {
        Self { mac: [0; 6] }
    }

    fn write_reg(&self, offset: usize, val: u32) {
        unsafe { ptr::write_volatile((ENET_BASE + offset) as *mut u32, val) }
    }

    fn read_reg(&self, offset: usize) -> u32 {
        unsafe { ptr::read_volatile((ENET_BASE + offset) as *const u32) }
    }

    pub fn init(&mut self, mac: [u8; 6]) {
        self.mac = mac;
        // Program MAC address.
        let ia0 = (mac[0] as u32)
            | ((mac[1] as u32) << 8)
            | ((mac[2] as u32) << 16)
            | ((mac[3] as u32) << 24);
        let ia1 = (mac[4] as u32) | ((mac[5] as u32) << 8);
        self.write_reg(REG_IA0, ia0);
        self.write_reg(REG_IA1, ia1);

        // Enable RX/TX, accept multicast (for 224.0.1.129), pad+CRC, full duplex.
        self.write_reg(
            REG_RCTL,
            SE_RCTL_RXEN | SE_RCTL_AMUL | SE_RCTL_PRMS | SE_RCTL_RSTFIFO,
        );
        self.write_reg(
            REG_TCTL,
            SE_TCTL_TXEN | SE_TCTL_PADEN | SE_TCTL_CRC | SE_TCTL_DUPLEX,
        );
        // Disable TX thresholding (0x3f == off in QEMU model).
        self.write_reg(REG_THR, 0x3f);
        // Clear any pending interrupts.
        self.write_reg(REG_RIS, 0xff);
    }
}

pub struct Lm3sRxToken {
    mac: [u8; 6],
}

impl phy::RxToken for Lm3sRxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        // Read the first word to get the total length (includes 2 len bytes + CRC).
        let first = unsafe { ptr::read_volatile((ENET_BASE + REG_DATA) as *const u32) };
        let total_len = (first as u16) as usize;
        let frame_len = total_len.saturating_sub(6);
        let mut buf = [0u8; MAX_FRAME_SIZE];
        let mut frame_idx = 0usize;
        let mut consumed = 0usize;

        let bytes = first.to_le_bytes();
        for b in bytes {
            consumed += 1;
            if consumed <= 2 {
                continue;
            }
            if frame_idx < frame_len && frame_idx < MAX_FRAME_SIZE {
                buf[frame_idx] = b;
                frame_idx += 1;
            }
        }

        while consumed < total_len {
            let word = unsafe { ptr::read_volatile((ENET_BASE + REG_DATA) as *const u32) };
            let bytes = word.to_le_bytes();
            for b in bytes {
                consumed += 1;
                if consumed <= 2 {
                    continue;
                }
                if consumed > total_len - 4 && total_len >= 4 {
                    // Skip CRC bytes.
                    continue;
                }
                if frame_idx < frame_len && frame_idx < MAX_FRAME_SIZE {
                    buf[frame_idx] = b;
                    frame_idx += 1;
                }
            }
        }

        // Drop frames we originated (hairpin/self-looped multicast).
        if frame_idx >= 12 && buf[6..12] == self.mac {
            return f(&[]);
        }

        f(&buf[..frame_idx])
    }
}

pub struct Lm3sTxToken;

impl phy::TxToken for Lm3sTxToken {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut frame = Frame::new_with_len(len.min(MAX_FRAME_SIZE));
        let result = f(&mut frame.data[..frame.len]);

        // Length field is payload without the 14-byte Ethernet header.
        let data_len = frame.len.saturating_sub(14) as u32;
        unsafe {
            // First DATA write packs length (low 16) and the first two payload bytes (high 16)
            // so that the QEMU model, which skips the first two bytes, sees a correctly aligned frame.
            let b0 = *frame.data.first().unwrap_or(&0) as u32;
            let b1 = *frame.data.get(1).unwrap_or(&0) as u32;
            let first_word = data_len | (b0 << 16) | (b1 << 24);
            ptr::write_volatile((ENET_BASE + REG_DATA) as *mut u32, first_word);

            // Write remaining payload starting at byte index 2.
            let mut idx = 2;
            while idx < frame.len {
                let mut word = 0u32;
                for i in 0..4 {
                    let byte = *frame.data.get(idx + i).unwrap_or(&0) as u32;
                    word |= byte << (8 * i);
                }
                ptr::write_volatile((ENET_BASE + REG_DATA) as *mut u32, word);
                idx += 4;
            }
            // Kick TX.
            ptr::write_volatile((ENET_BASE + REG_TR) as *mut u32, 1);
        }

        result
    }
}

impl Device for Lm3sEth {
    type RxToken<'a> = Lm3sRxToken;
    type TxToken<'a> = Lm3sTxToken;

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = MAX_FRAME_SIZE;
        caps.medium = Medium::Ethernet;
        caps.checksum = ChecksumCapabilities::default();
        caps
    }

    fn receive(
        &mut self,
        _timestamp: SmolInstant,
    ) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let np = self.read_reg(REG_NP);
        (np != 0).then_some((Lm3sRxToken { mac: self.mac }, Lm3sTxToken))
    }

    fn transmit(&mut self, _timestamp: SmolInstant) -> Option<Self::TxToken<'_>> {
        Some(Lm3sTxToken)
    }
}

impl<D: Device> PtpNetwork<D> {
    pub fn new(
        mut device: D,
        storage: &'static mut PtpStorage,
        mac: [u8; 6],
        ipv4: Ipv4Address,
        now: SmolInstant,
    ) -> Self {
        let caps = device.capabilities();
        assert_eq!(caps.medium, Medium::Ethernet, "device must be Ethernet");
        let hw_addr = HardwareAddress::Ethernet(EthernetAddress(mac));

        let mut iface = Interface::new(Config::new(hw_addr), &mut device, now);
        iface.update_ip_addrs(|addrs| {
            let _ = addrs.push(IpCidr::Ipv4(Ipv4Cidr::new(ipv4, 24)));
        });
        let _ = iface.join_multicast_group(IpAddress::v4(224, 0, 1, 129));

        let mut sockets = SocketSet::new(&mut storage.socket_storage[..]);

        let event_rx_buffer = PacketBuffer::new(
            &mut storage.event_rx_meta[..],
            &mut storage.event_rx_buf[..],
        );
        let event_tx_buffer = PacketBuffer::new(
            &mut storage.event_tx_meta[..],
            &mut storage.event_tx_buf[..],
        );
        let mut event_socket = UdpSocket::new(event_rx_buffer, event_tx_buffer);
        let _ = event_socket.bind(IpListenEndpoint {
            addr: None,
            port: 319,
        });

        let general_rx_buffer = PacketBuffer::new(
            &mut storage.general_rx_meta[..],
            &mut storage.general_rx_buf[..],
        );
        let general_tx_buffer = PacketBuffer::new(
            &mut storage.general_tx_meta[..],
            &mut storage.general_tx_buf[..],
        );
        let mut general_socket = UdpSocket::new(general_rx_buffer, general_tx_buffer);
        let _ = general_socket.bind(IpListenEndpoint {
            addr: None,
            port: 320,
        });

        let event_handle = sockets.add(event_socket);
        let general_handle = sockets.add(general_socket);

        Self {
            iface,
            device,
            sockets: UnsafeCell::new(PtpSockets {
                set: sockets,
                event_handle,
                general_handle,
            }),
        }
    }

    pub fn physical_port(&self) -> SmoltcpPhysicalPort {
        SmoltcpPhysicalPort {
            sockets: &self.sockets as *const _,
        }
    }

    pub fn poll(&mut self, now: SmolInstant, rx_out: &mut Vec<RxPacket, MAX_RX_PACKETS>) {
        let sockets_ptr = self.sockets.get();

        unsafe {
            let sockets = &mut *sockets_ptr;
            let _ = self.iface.poll(now, &mut self.device, &mut sockets.set);
        }

        unsafe {
            let sockets = &mut *sockets_ptr;
            Self::drain_socket(
                &mut sockets.set,
                sockets.event_handle,
                RxKind::Event,
                rx_out,
            );
            Self::drain_socket(
                &mut sockets.set,
                sockets.general_handle,
                RxKind::General,
                rx_out,
            );
        }
    }

    fn drain_socket(
        sockets: &mut SocketSet<'_>,
        handle: SocketHandle,
        kind: RxKind,
        rx_out: &mut Vec<RxPacket, MAX_RX_PACKETS>,
    ) {
        let socket = sockets.get_mut::<UdpSocket>(handle);
        while let Ok((bytes, _meta)) = socket.recv() {
            if rx_out.is_full() {
                break;
            }
            let mut data = [0u8; MAX_FRAME_SIZE];
            let len = bytes.len().min(MAX_FRAME_SIZE);
            data[..len].copy_from_slice(&bytes[..len]);
            rx_out.push(RxPacket { len, data, kind }).ok();
        }
    }
}

impl PhysicalPort for SmoltcpPhysicalPort {
    fn send_event(&self, buf: &[u8]) -> SendResult {
        self.send(buf, 319)
    }

    fn send_general(&self, buf: &[u8]) -> SendResult {
        self.send(buf, 320)
    }
}

impl SmoltcpPhysicalPort {
    fn send(&self, buf: &[u8], port: u16) -> SendResult {
        let sockets = unsafe { &mut *(*self.sockets).get() };
        let socket = sockets.set.get_mut::<UdpSocket>(match port {
            319 => sockets.event_handle,
            _ => sockets.general_handle,
        });
        let dest = IpEndpoint::new(IpAddress::v4(224, 0, 1, 129), port);
        socket
            .send_slice(buf, dest)
            .map(|_| ())
            .map_err(|_| SendError)
    }
}
