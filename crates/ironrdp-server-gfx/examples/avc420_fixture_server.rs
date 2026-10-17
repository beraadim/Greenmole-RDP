//! Loopback-only synthetic RDP producer for Guacamole/browser integration QA.
//!
//! First export synthetic frames with macrdp-encode's export_avc420_fixture.
//! Run with: <fixture-directory> <openh264|videotoolbox> <port> <cert.der> <key.der> [avc444]
//! Supply a disposable self-signed DER certificate and PKCS#8 DER private key.
//! It never opens a display, captures pixels, or accepts input. TLS-only RDP
//! uses the public fixture credentials synthetic-fixture/synthetic-fixture
//! and is restricted to the IPv4 loopback interface.

use std::num::{NonZeroU16, NonZeroUsize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use ironrdp_server::gfx::GfxState;
use ironrdp_server::{
    BitmapUpdate, Credentials, DesktopSize, DisplayUpdate, GfxFrameUpdate, GfxUncompressedUpdate,
    PixelFormat, RdpServer, RdpServerDisplay, RdpServerDisplayUpdates, UncompressedRect,
};
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_rustls::{rustls, TlsAcceptor};

const WIDTH: u16 = 320;
const HEIGHT: u16 = 180;

struct FixtureFrame {
    main: Bytes,
    auxiliary: Option<Bytes>,
    pixels: Option<Bytes>,
}

struct SyntheticDisplay {
    frames: Arc<Vec<FixtureFrame>>,
    gfx: Arc<Mutex<GfxState>>,
}

struct SyntheticUpdates {
    frames: Arc<Vec<FixtureFrame>>,
    gfx: Arc<Mutex<GfxState>>,
    index: usize,
    seeded: bool,
    reported: bool,
}

fn synthetic_pixels(index: usize) -> Bytes {
    let mut pixels = vec![0u8; usize::from(WIDTH) * usize::from(HEIGHT) * 4];
    for y in 0..usize::from(HEIGHT) {
        for x in 0..usize::from(WIDTH) {
            let color = if x >= 12 + index * 15 && x < 30 + index * 15 && (30..80).contains(&y) {
                [255, 255, 255, 255]
            } else if x < usize::from(WIDTH) / 2 && y < usize::from(HEIGHT) / 2 {
                [16, 16, 220, 255]
            } else if y < usize::from(HEIGHT) / 2 {
                [16, 220, 16, 255]
            } else if x < usize::from(WIDTH) / 2 {
                [220, 16, 16, 255]
            } else {
                [28, 28, 28, 255]
            };
            let offset = (y * usize::from(WIDTH) + x) * 4;
            pixels[offset..offset + 4].copy_from_slice(&color);
        }
    }
    pixels.into()
}

#[async_trait::async_trait]
impl RdpServerDisplay for SyntheticDisplay {
    async fn size(&mut self) -> DesktopSize {
        DesktopSize {
            width: WIDTH,
            height: HEIGHT,
        }
    }

    async fn updates(&mut self) -> Result<Box<dyn RdpServerDisplayUpdates>> {
        Ok(Box::new(SyntheticUpdates {
            frames: Arc::clone(&self.frames),
            gfx: Arc::clone(&self.gfx),
            index: 0,
            seeded: false,
            reported: false,
        }))
    }
}

#[async_trait::async_trait]
impl RdpServerDisplayUpdates for SyntheticUpdates {
    async fn next_update(&mut self) -> Result<Option<DisplayUpdate>> {
        let (gfx_ready, avc, use_444, bitmap, browser) = loop {
            // No state is consumed until after this await, preserving the
            // display provider's cancellation-safety contract.
            tokio::time::sleep(Duration::from_millis(50)).await;
            let state = self.gfx.lock().unwrap();
            let bitmap = !state.session_uses_gfx || state.bitmap_fallback_mode;
            if state.has_gfx_channel() || bitmap {
                break (
                    state.has_gfx_channel(),
                    state.avc420_supported,
                    state.use_avc444(),
                    bitmap,
                    state.browser_video(),
                );
            }
        };
        if !self.reported {
            eprintln!("Synthetic display ready: gfx={gfx_ready} avc420={avc} avc444={use_444} bitmap={bitmap} browser={browser}");
            self.reported = true;
        }
        if bitmap {
            let data = self.frames[self.index]
                .pixels
                .clone()
                .unwrap_or_else(|| synthetic_pixels(self.index));
            self.index = (self.index + 1) % self.frames.len();
            return Ok(Some(DisplayUpdate::Bitmap(BitmapUpdate {
                x: 0,
                y: 0,
                width: NonZeroU16::new(WIDTH).unwrap(),
                height: NonZeroU16::new(HEIGHT).unwrap(),
                format: PixelFormat::BgrA32,
                data,
                stride: NonZeroUsize::new(usize::from(WIDTH) * 4).unwrap(),
                dirty_rects: Vec::new(),
            })));
        }
        // A paired fixture shares references between both views. If this
        // client cannot accept 444, serve images rather than dropping half
        // the reference chain and sending invalid main-only delta frames.
        if !self.seeded || !avc || (self.frames[0].auxiliary.is_some() && !use_444) {
            // Exercise the startup ResetGraphics + raw image seed which must
            // remain legal before the browser's first video access unit.
            self.seeded = true;
            return Ok(Some(DisplayUpdate::GfxUncompressed(
                GfxUncompressedUpdate {
                    width: WIDTH,
                    height: HEIGHT,
                    rects: vec![UncompressedRect {
                        x: 0,
                        y: 0,
                        width: WIDTH,
                        height: HEIGHT,
                        pixel_data: self.frames[0]
                            .pixels
                            .clone()
                            .unwrap_or_else(|| synthetic_pixels(0)),
                    }],
                },
            )));
        }
        let index = self.index;
        self.index = (self.index + 1) % self.frames.len();
        Ok(Some(DisplayUpdate::GfxFrame(GfxFrameUpdate {
            h264_data: self.frames[index].main.clone(),
            width: WIDTH,
            height: HEIGHT,
            enc_width: 320,
            enc_height: 192,
            is_keyframe: index == 0 || index == 4,
            h264_aux: if use_444 {
                self.frames[index].auxiliary.clone()
            } else {
                None
            },
        })))
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let directory = PathBuf::from(
        arguments
            .next()
            .context("Pass synthetic fixture directory")?,
    );
    let backend = arguments.next().context("Pass openh264 or videotoolbox")?;
    let backend = backend.to_str().context("Backend must be ASCII")?;
    anyhow::ensure!(
        matches!(backend, "openh264" | "videotoolbox"),
        "Unknown fixture backend"
    );
    let port: u16 = arguments
        .next()
        .context("Pass loopback test port")?
        .to_str()
        .context("Invalid port")?
        .parse()
        .context("Invalid port")?;
    let certificate = PathBuf::from(
        arguments
            .next()
            .context("Pass disposable DER certificate")?,
    );
    let private_key = PathBuf::from(arguments.next().context("Pass disposable PKCS#8 DER key")?);
    let codec = arguments.next();
    let mode_444 = match codec.as_deref().and_then(std::ffi::OsStr::to_str) {
        None | Some("avc420") => false,
        Some("avc444") => true,
        _ => anyhow::bail!("Codec must be avc420 or avc444"),
    };
    anyhow::ensure!(arguments.next().is_none(), "Unexpected arguments");
    anyhow::ensure!(port != 0, "Use an explicit nonzero test port");
    let tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(std::fs::read(certificate)?)],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(std::fs::read(private_key)?)),
        )?;
    let tls = TlsAcceptor::from(Arc::new(tls));
    let read = |file: String| -> Result<Bytes> {
        let path = directory.join(file);
        let length = path.metadata()?.len();
        anyhow::ensure!(
            length > 0 && length <= 8 * 1024 * 1024,
            "Invalid fixture size"
        );
        Ok(Bytes::from(std::fs::read(path)?))
    };
    let mut frames = Vec::new();
    for index in 0..6 {
        let base = format!("{backend}_320x180_{index}");
        let (main, auxiliary, pixels) = if mode_444 {
            let pixels = read(format!("{base}.bgra"))?;
            anyhow::ensure!(
                pixels.len() == usize::from(WIDTH) * usize::from(HEIGHT) * 4,
                "Invalid fixture pixel dimensions"
            );
            (
                read(format!("{base}.main.h264"))?,
                Some(read(format!("{base}.aux.h264"))?),
                Some(pixels),
            )
        } else {
            (read(format!("{base}.h264"))?, None, None)
        };
        frames.push(FixtureFrame {
            main,
            auxiliary,
            pixels,
        });
    }
    let gfx = Arc::new(Mutex::new(GfxState::new(WIDTH, HEIGHT, mode_444, false)));
    let display = SyntheticDisplay {
        frames: Arc::new(frames),
        gfx: Arc::clone(&gfx),
    };
    let mut server = RdpServer::builder()
        .with_addr(([127, 0, 0, 1], port))
        .with_tls(tls)
        .with_no_input()
        .with_display_handler(display)
        .build();
    server.set_gfx_state(gfx);
    // The production acceptor deliberately rejects missing credentials for
    // TLS ClientInfo authentication. These public fixture-only values cannot
    // authenticate against the user's real server or control a local desktop.
    server.set_credentials(Some(Credentials {
        username: "synthetic-fixture".into(),
        password: "synthetic-fixture".into(),
        domain: None,
    }));
    eprintln!(
        "Synthetic-only TLS RDP fixture on 127.0.0.1:{port} ({backend}); no desktop capture or input"
    );
    let listener =
        tokio::net::TcpListener::bind(std::net::SocketAddr::from(([127, 0, 0, 1], port))).await?;
    loop {
        let (stream, peer) = listener.accept().await?;
        eprintln!("Synthetic RDP test connection from {peer}");
        if let Err(error) = server.run_connection(stream).await {
            eprintln!("Synthetic RDP test connection ended: {error:#}");
        }
    }
}
