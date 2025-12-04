use std::io;
use std::sync::OnceLock;

use prometheus::{Encoder, Gauge, TextEncoder};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

static OFFSET_GAUGE: OnceLock<Gauge> = OnceLock::new();

pub fn offset_gauge() -> &'static Gauge {
    OFFSET_GAUGE.get_or_init(|| {
        let gauge = Gauge::new(
            "rptp_offset_from_master_seconds",
            "Offset from master as seen by the local clock",
        )
        .expect("create gauge");

        prometheus::default_registry()
            .register(Box::new(gauge.clone()))
            .expect("register gauge");

        gauge
    })
}

pub async fn run_metrics_server(addr: &str) -> io::Result<()> {
    let listener = TcpListener::bind(addr).await?;

    loop {
        let (mut socket, _) = listener.accept().await?;

        // Read and ignore the incoming HTTP request; we always respond with metrics.
        let mut _buf = [0u8; 1024];
        let _ = socket.read(&mut _buf).await?;

        let metric_families = prometheus::default_registry().gather();
        let mut body = Vec::new();
        let encoder = TextEncoder::new();
        encoder
            .encode(&metric_families, &mut body)
            .map_err(|e| io::Error::other(e.to_string()))?;

        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\n\r\n",
            encoder.format_type(),
            body.len()
        );

        socket.write_all(response.as_bytes()).await?;
        socket.write_all(&body).await?;
    }
}
