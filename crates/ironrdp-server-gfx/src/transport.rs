//! Socket policy shared by the application listener and direct connections.

use anyhow::{Context as _, Result};
use socket2::{SockRef, TcpKeepalive};
use tokio::net::TcpStream;
use tracing::{info, warn};

pub(super) fn configure_stream(stream: &TcpStream) -> Result<()> {
    // Interactive updates should not wait for another small TCP write or an
    // acknowledgement. Set this on the accepted socket before TLS framing.
    stream
        .set_nodelay(true)
        .context("Failed to enable TCP_NODELAY on RDP connection")?;

    // TCP probes detect unreachable peers without requiring RDP traffic from
    // a quiet desktop. This does not add a read timeout or alter the separate
    // keyboard/mouse inactivity policy and fixed output deadlines.
    let socket = SockRef::from(stream);
    let keepalive = TcpKeepalive::new();
    // These are the application and CI platforms. Other supported socket2
    // targets still enable probes using their native defaults.
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    let keepalive = keepalive
        .with_time(std::time::Duration::from_secs(60))
        .with_interval(std::time::Duration::from_secs(15))
        .with_retries(4);

    match socket.set_tcp_keepalive(&keepalive) {
        Ok(()) => {
            #[cfg(any(target_os = "macos", target_os = "linux", windows))]
            info!(
                keepalive_idle_seconds = 60,
                keepalive_interval_seconds = 15,
                keepalive_retries = 4,
                "TCP keepalive configured"
            );
            #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
            info!("TCP keepalive enabled with platform defaults");
        }
        Err(error) => {
            // Keep an otherwise usable connection when a platform rejects an
            // optional setting. Some preceding settings may already apply;
            // do not report that the complete policy was installed.
            warn!(
                socket_error_code = error.raw_os_error(),
                "TCP keepalive options could not be fully applied"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn accepted_socket_has_keepalive_without_read_or_write_timeouts() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let (client, accepted) = tokio::join!(
            TcpStream::connect(listener.local_addr().unwrap()),
            listener.accept()
        );
        let mut client = client.unwrap();
        let (mut accepted, _) = accepted.unwrap();
        accepted.set_nodelay(false).unwrap();
        SockRef::from(&accepted).set_keepalive(false).unwrap();
        assert!(!accepted.nodelay().unwrap());
        configure_stream(&accepted).unwrap();
        // The listener and direct-connection path can both apply the policy;
        // applying it again must preserve exactly the same settings.
        configure_stream(&accepted).unwrap();
        assert!(accepted.nodelay().unwrap());
        {
            let socket = SockRef::from(&accepted);
            assert!(socket.keepalive().unwrap());
            assert_eq!(socket.read_timeout().unwrap(), None);
            assert_eq!(socket.write_timeout().unwrap(), None);
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            {
                assert_eq!(
                    socket.tcp_keepalive_time().unwrap(),
                    std::time::Duration::from_secs(60)
                );
                assert_eq!(
                    socket.tcp_keepalive_interval().unwrap(),
                    std::time::Duration::from_secs(15)
                );
                assert_eq!(socket.tcp_keepalive_retries().unwrap(), 4);
            }
            #[cfg(windows)]
            assert_eq!(socket.tcp_keepalive_retries().unwrap(), 4);
        }
        // SockRef only borrows the descriptor; releasing it must leave the
        // accepted stream usable in both directions.
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            client.write_all(b"quiet desktop").await.unwrap();
            let mut request = [0; 13];
            accepted.read_exact(&mut request).await.unwrap();
            assert_eq!(&request, b"quiet desktop");
            accepted.write_all(b"alive").await.unwrap();
            let mut response = [0; 5];
            client.read_exact(&mut response).await.unwrap();
            assert_eq!(&response, b"alive");
        })
        .await
        .unwrap();
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
