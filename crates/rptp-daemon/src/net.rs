//! Tokio networking helpers used by the daemon runtime.
//!
//! The daemon’s transport is intentionally narrow: it mostly needs a UDP socket that can receive
//! datagrams and send PTP event/general messages, plus a lightweight fake for unit tests.
//!
//! [`MulticastSocket`] binds UDP port 319 (event) or 320 (general) and joins the standard PTPv2
//! IPv4 multicast group (`224.0.1.129`) with TTL 1 and multicast loopback disabled.
//!
//! [`LoopbackSocket`] provides an in-memory pair of sockets for demos and tests: sends on one
//! are received on the other, with no real network I/O.

use std::future::{Future, pending};
use std::io::Result;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::rc::Rc;

use tokio::net::UdpSocket;
use tokio::sync::{Mutex, mpsc};

/// Minimal async UDP socket interface used by the daemon IO loop.
///
/// Implementations are expected to be cheap to clone/share and to work in Tokio tasks.
pub trait NetworkSocket {
    /// Receive a datagram into `buf`, returning the number of bytes written and the sender address.
    fn recv<'a>(
        &'a self,
        buf: &'a mut [u8],
    ) -> impl Future<Output = Result<(usize, SocketAddr)>> + 'a;

    /// Send a datagram to the socket’s configured destination.
    fn send<'a>(&'a self, bytes: &'a [u8]) -> impl Future<Output = Result<usize>> + 'a;

    /// Best-effort, non-async send to the socket’s configured destination.
    ///
    /// This is used where the daemon wants to avoid `.await` in contexts that already hold locks
    /// or are otherwise latency-sensitive. It uses the underlying `UdpSocket::try_send_to`.
    fn try_send(&self, bytes: &[u8]) -> Result<usize>;
}

/// A UDP socket bound to the PTP multicast group for event or general messages.
///
/// This socket:
/// - binds to `0.0.0.0:<port>` (to receive multicast), and
/// - sends to the configured PTP multicast destination.
#[derive(Debug)]
pub struct MulticastSocket {
    socket: UdpSocket,
    dest: SocketAddrV4,
}

impl MulticastSocket {
    /// PTPv2 IPv4 multicast group (IEEE 1588).
    const PTP_MCAST: Ipv4Addr = Ipv4Addr::new(224, 0, 1, 129);

    /// Bind a socket for PTP event messages (UDP port 319).
    pub async fn event() -> Result<Self> {
        Self::bind_v4(Self::PTP_MCAST, 319).await
    }

    /// Bind a socket for PTP general messages (UDP port 320).
    pub async fn general() -> Result<Self> {
        Self::bind_v4(Self::PTP_MCAST, 320).await
    }

    async fn bind_v4(multicast: Ipv4Addr, port: u16) -> Result<Self> {
        let socket = UdpSocket::bind(("0.0.0.0", port)).await?;
        socket.join_multicast_v4(multicast, Ipv4Addr::UNSPECIFIED)?;
        socket.set_multicast_loop_v4(false)?;
        socket.set_multicast_ttl_v4(1)?;
        Ok(Self {
            socket,
            dest: SocketAddrV4::new(multicast, port),
        })
    }
}

impl NetworkSocket for MulticastSocket {
    async fn recv(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        self.socket.recv_from(buf).await
    }

    async fn send(&self, bytes: &[u8]) -> Result<usize> {
        self.socket.send_to(bytes, self.dest).await
    }

    fn try_send(&self, bytes: &[u8]) -> Result<usize> {
        let dest = SocketAddr::V4(self.dest);
        self.socket.try_send_to(bytes, dest)
    }
}

/// In-memory socket pair for demos and in-process tests.
///
/// `pair()` returns two sockets: bytes sent on one are received on the other. Uses channels
/// under the hood, so no real network I/O. Useful for multi-node scenarios (e.g. GM and slave,
/// or boundary clock topologies) without binding to multicast or multiple interfaces.
#[derive(Debug)]
pub struct LoopbackSocket {
    rx: Mutex<mpsc::UnboundedReceiver<Vec<u8>>>,
    tx: mpsc::UnboundedSender<Vec<u8>>,
}

impl LoopbackSocket {
    /// Create a connected pair. Sends on the first are received on the second, and vice versa.
    pub fn pair() -> (Self, Self) {
        let (a_tx, a_rx) = mpsc::unbounded_channel();
        let (b_tx, b_rx) = mpsc::unbounded_channel();

        let a = LoopbackSocket {
            rx: Mutex::new(a_rx),
            tx: b_tx,
        };
        let b = LoopbackSocket {
            rx: Mutex::new(b_rx),
            tx: a_tx,
        };
        (a, b)
    }
}

impl NetworkSocket for LoopbackSocket {
    async fn recv(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        loop {
            let mut rx = self.rx.lock().await;
            if let Some(msg) = rx.recv().await {
                let len = msg.len().min(buf.len());
                buf[..len].copy_from_slice(&msg[..len]);
                return Ok((
                    len,
                    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)),
                ));
            } else {
                // Peer gone: park until task is cancelled by shutdown/ctrl-C.
                pending::<()>().await;
            }
        }
    }

    async fn send(&self, bytes: &[u8]) -> Result<usize> {
        self.try_send(bytes)
    }

    fn try_send(&self, bytes: &[u8]) -> Result<usize> {
        let len = bytes.len();
        // Best-effort: drop silently if receiver has gone away.
        let _ = self.tx.send(bytes.to_vec());
        Ok(len)
    }
}

/// Fake `NetworkSocket` implementation that captures transmitted bytes into a channel.
///
/// This is used by unit tests that want to assert what would have been sent without binding real
/// UDP sockets. Receiving is not supported (it awaits forever).
pub struct FakeNetworkSocket {
    tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
}

impl FakeNetworkSocket {
    /// Create a fake socket plus the receiver that yields sent datagrams.
    pub fn new() -> (Self, tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (Self { tx }, rx)
    }
}

impl NetworkSocket for FakeNetworkSocket {
    async fn recv<'a>(&'a self, _buf: &'a mut [u8]) -> Result<(usize, SocketAddr)> {
        std::future::pending::<()>().await;
        unreachable!()
    }

    async fn send(&self, bytes: &[u8]) -> Result<usize> {
        let bytes = bytes.to_vec();
        let len = bytes.len();
        let tx = self.tx.clone();
        tx.send(bytes)
            .map_err(|_| std::io::Error::other("send failed"))?;
        Ok(len)
    }

    fn try_send(&self, bytes: &[u8]) -> Result<usize> {
        let len = bytes.len();
        self.tx
            .send(bytes.to_vec())
            .map_err(|_| std::io::Error::other("send failed"))?;
        Ok(len)
    }
}

impl NetworkSocket for Rc<FakeNetworkSocket> {
    async fn recv(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        self.as_ref().recv(buf).await
    }

    async fn send(&self, bytes: &[u8]) -> Result<usize> {
        self.as_ref().send(bytes).await
    }

    fn try_send(&self, bytes: &[u8]) -> Result<usize> {
        self.as_ref().try_send(bytes)
    }
}
