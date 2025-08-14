use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use tokio::net::UdpSocket;

#[derive(Debug)]
pub struct MulticastPort {
    socket: UdpSocket,
    dest: SocketAddrV4,
}

impl MulticastPort {
    pub async fn bind_v4(multicast: Ipv4Addr, port: u16) -> io::Result<Self> {
        let socket = UdpSocket::bind(("0.0.0.0", port)).await?;
        socket.join_multicast_v4(multicast, Ipv4Addr::UNSPECIFIED)?;
        socket.set_multicast_loop_v4(false)?;
        socket.set_multicast_ttl_v4(1)?;
        Ok(Self {
            socket,
            dest: SocketAddrV4::new(multicast, port),
        })
    }

    pub async fn recv(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.socket.recv_from(buf).await
    }

    pub async fn send<B: AsRef<[u8]>>(&self, bytes: B) -> io::Result<()> {
        self.socket
            .send_to(bytes.as_ref(), self.dest)
            .await
            .map(|_| ())
    }
}
