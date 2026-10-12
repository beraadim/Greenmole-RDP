//! Socket policy shared by the application listener and direct connections.

use anyhow::{Context as _, Result};
use tokio::net::TcpStream;

pub(super) fn configure_stream(stream: &TcpStream) -> Result<()> {
    // Interactive updates should not wait for another small TCP write or an
    // acknowledgement. Set this on the accepted socket before TLS framing.
    stream
        .set_nodelay(true)
        .context("Failed to enable TCP_NODELAY on RDP connection")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn accepted_socket_has_nodelay_enabled() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let (client, accepted) = tokio::join!(
            TcpStream::connect(listener.local_addr().unwrap()),
            listener.accept()
        );
        let _client = client.unwrap();
        let (accepted, _) = accepted.unwrap();
        accepted.set_nodelay(false).unwrap();
        assert!(!accepted.nodelay().unwrap());
        configure_stream(&accepted).unwrap();
        assert!(accepted.nodelay().unwrap());
    }

    /// Explicit transport experiment, not an RDP or end-to-end latency claim.
    /// Run with `cargo test -p ironrdp-server --lib nodelay_small_update_microbenchmark -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "manual loopback transport experiment; no unstable performance assertion"]
    async fn nodelay_small_update_microbenchmark() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        const WARMUP: usize = 20;
        const SAMPLES: usize = 200;
        for enabled in [false, true] {
            let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .await
                .unwrap();
            let (client, accepted) = tokio::join!(
                TcpStream::connect(listener.local_addr().unwrap()),
                listener.accept()
            );
            let mut client = client.unwrap();
            client.set_nodelay(true).unwrap();
            let (mut server, _) = accepted.unwrap();
            server.set_nodelay(enabled).unwrap();
            let transmit = async {
                let mut samples = Vec::with_capacity(SAMPLES);
                for index in 0..WARMUP + SAMPLES {
                    let now = std::time::Instant::now();
                    server.write_all(&[42]).await.unwrap();
                    tokio::task::yield_now().await;
                    server.write_all(&[42; 63]).await.unwrap();
                    let mut ack = [0];
                    server.read_exact(&mut ack).await.unwrap();
                    if index >= WARMUP {
                        samples.push(now.elapsed().as_secs_f64() * 1000.0);
                    }
                }
                samples
            };
            let receive = async {
                for _ in 0..WARMUP + SAMPLES {
                    let mut bytes = [0; 64];
                    client.read_exact(&mut bytes).await.unwrap();
                    assert_eq!(bytes, [42; 64]);
                    client.write_all(&[1]).await.unwrap();
                }
            };
            let (mut samples, ()) =
                tokio::time::timeout(std::time::Duration::from_secs(60), async {
                    tokio::join!(transmit, receive)
                })
                .await
                .unwrap();
            samples.sort_by(f64::total_cmp);
            eprintln!("TCP loopback fragmented 64-byte update: nodelay={enabled}, samples={SAMPLES}, p50_ms={:.3}, p95_ms={:.3}", samples[SAMPLES / 2], samples[SAMPLES * 95 / 100]);
        }
    }
}
