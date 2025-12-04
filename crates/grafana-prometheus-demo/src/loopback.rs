use rptp_daemon::net::NetworkSocket;
use std::future::pending;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use tokio::sync::{Mutex, mpsc};

pub struct LoopbackSocket {
    rx: Mutex<mpsc::UnboundedReceiver<Vec<u8>>>,
    tx: mpsc::UnboundedSender<Vec<u8>>,
}

impl LoopbackSocket {
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
    async fn recv(&self, buf: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
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
                // Peer gone: park until task is cancelled by shutdown/ctrl‑C.
                pending::<()>().await;
            }
        }
    }

    async fn send(&self, bytes: &[u8]) -> std::io::Result<usize> {
        self.try_send(bytes)
    }

    fn try_send(&self, bytes: &[u8]) -> std::io::Result<usize> {
        let len = bytes.len();
        // Best‑effort: drop silently if receiver has gone away.
        let _ = self.tx.send(bytes.to_vec());
        Ok(len)
    }
}
