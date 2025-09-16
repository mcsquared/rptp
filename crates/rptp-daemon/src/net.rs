use std::future::Future;
use std::io::Result;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use tokio::net::UdpSocket;

pub trait NetPort {
    fn recv<'a>(
        &'a self,
        buf: &'a mut [u8],
    ) -> impl Future<Output = Result<(usize, SocketAddr)>> + 'a;

    fn send<'a>(&'a self, bytes: &'a [u8]) -> impl Future<Output = Result<usize>> + 'a;
}

#[derive(Debug)]
pub struct MulticastPort {
    socket: UdpSocket,
    dest: SocketAddrV4,
}

impl MulticastPort {
    const PTP_MCAST: Ipv4Addr = Ipv4Addr::new(224, 0, 1, 129);

    pub async fn ptp_event_port() -> Result<Self> {
        Self::bind_v4(Self::PTP_MCAST, 319).await
    }

    pub async fn ptp_general_port() -> Result<Self> {
        Self::bind_v4(Self::PTP_MCAST, 320).await
    }

    pub async fn ptp_event_testing_port() -> Result<Self> {
        Self::bind_v4(Self::PTP_MCAST, 5319).await
    }

    pub async fn ptp_general_testing_port() -> Result<Self> {
        Self::bind_v4(Self::PTP_MCAST, 5320).await
    }

    pub async fn bind_v4(multicast: Ipv4Addr, port: u16) -> Result<Self> {
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

impl NetPort for MulticastPort {
    fn recv<'a>(
        &'a self,
        buf: &'a mut [u8],
    ) -> impl Future<Output = Result<(usize, SocketAddr)>> + 'a {
        self.socket.recv_from(buf)
    }

    fn send<'a>(&'a self, bytes: &'a [u8]) -> impl Future<Output = Result<usize>> + 'a {
        self.socket.send_to(bytes, self.dest)
    }
}

pub struct FakeNetPort {
    tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
}

impl FakeNetPort {
    pub fn new() -> (Self, tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (Self { tx }, rx)
    }
}

impl NetPort for FakeNetPort {
    fn recv<'a>(
        &'a self,
        _buf: &'a mut [u8],
    ) -> impl Future<Output = Result<(usize, SocketAddr)>> + 'a {
        async {
            std::future::pending::<()>().await;
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "not implemented",
            ))
        }
    }

    fn send<'a>(&'a self, bytes: &'a [u8]) -> impl Future<Output = Result<usize>> + 'a {
        let bytes = bytes.to_vec();
        let len = bytes.len();
        let tx = self.tx.clone();
        async move {
            tx.send(bytes)
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "send failed"))?;
            Ok(len)
        }
    }
}
