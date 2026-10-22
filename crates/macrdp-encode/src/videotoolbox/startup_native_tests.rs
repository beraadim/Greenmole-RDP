//! Opt-in hardware check using generated pixels; never captures the desktop.

use super::*;
use openh264::formats::YUVSource;
use std::time::{Duration, Instant};

#[test]
#[ignore = "explicit hardware AVC420 portrait encode/decode check; no desktop capture"]
fn iphone_portrait_avc420_startup_and_decode() {
    const WIDTH: u32 = 1184;
    const HEIGHT: u32 = 1808;
    let mut pixels = vec![0u8; WIDTH as usize * HEIGHT as usize * 4];
    for (index, pixel) in pixels.chunks_exact_mut(4).enumerate() {
        let x = index % WIDTH as usize;
        let y = index / WIDTH as usize;
        pixel.copy_from_slice(&[((x / 64) * 11) as u8, ((y / 64) * 7) as u8, 120, 255]);
    }
    for attempt in 0..2 {
        let started = Instant::now();
        let mut encoder = VtEncoder::new_with_quality(
            WIDTH,
            HEIGHT,
            30.0,
            16_000_000,
            false,
            Quality::HighQuality,
        )
        .unwrap();
        let startup = started.elapsed();
        encoder.force_keyframe();
        let encoded = encoder
            .encode_bgra(&pixels, WIDTH, HEIGHT, WIDTH as usize * 4)
            .unwrap();
        assert!(encoded.is_keyframe);
        assert!(!encoded.data.is_empty() && encoded.data.len() < pixels.len());
        let mut decoder = openh264::decoder::Decoder::new().unwrap();
        let decoded = decoder
            .decode(&encoded.data)
            .unwrap()
            .expect("IDR must decode");
        assert_eq!(decoded.dimensions(), (WIDTH as usize, HEIGHT as usize));
        eprintln!(
            "portrait AVC420 attempt={} startup_ms={} encoded_bytes={} raw_bytes={}",
            attempt + 1,
            startup.as_millis(),
            encoded.data.len(),
            pixels.len()
        );
        drop(encoder);
        let deadline = Instant::now() + Duration::from_secs(3);
        while native_cleanup_pending() {
            assert!(
                Instant::now() < deadline,
                "Portrait codec retirement did not finish"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
