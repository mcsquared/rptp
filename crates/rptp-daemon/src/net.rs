use std::future::Future;
use std::io::Result;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::rc::Rc;

use tokio::net::UdpSocket;

pub trait NetworkSocket {
    fn recv<'a>(
        &'a self,
        buf: &'a mut [u8],
    ) -> impl Future<Output = Result<(usize, SocketAddr)>> + 'a;

    fn send<'a>(&'a self, bytes: &'a [u8]) -> impl Future<Output = Result<usize>> + 'a;
    fn try_send(&self, bytes: &[u8]) -> Result<usize>;
}

#[derive(Debug)]
pub struct MulticastSocket {
    socket: UdpSocket,
    dest: SocketAddrV4,
}

impl MulticastSocket {
    const PTP_MCAST: Ipv4Addr = Ipv4Addr::new(224, 0, 1, 129);

    pub async fn event() -> Result<Self> {
        Self::bind_v4(Self::PTP_MCAST, 319).await
    }

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

pub struct FakeNetworkSocket {
    tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
}

impl FakeNetworkSocket {
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
