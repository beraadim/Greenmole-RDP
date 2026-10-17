//! Synthetic native callback timing probe. Does not capture screens or send input.
use anyhow::Result;
use macrdp_encode::{create_encoder, Avc444EncodeError, EncoderPreference, Quality};
use std::time::Instant;
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let width: u32 = args.get(1).map(|s| s.parse()).transpose()?.unwrap_or(2560);
    let height: u32 = args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(1264);
    let fps: f32 = args.get(3).map(|s| s.parse()).transpose()?.unwrap_or(120.0);
    let count: u32 = args.get(4).map(|s| s.parse()).transpose()?.unwrap_or(12);
    anyhow::ensure!(
        args.len() <= 5,
        "Usage: probe_avc444_timing [width height fps frames]"
    );
    anyhow::ensure!(
        width > 0
            && height > 0
            && width <= 4096
            && height <= 4096
            && width % 2 == 0
            && height % 2 == 0,
        "Dimensions must be even and within 2..4096"
    );
    anyhow::ensure!(
        fps.is_finite() && (1.0..=240.0).contains(&fps),
        "Frame rate must be within 1..240"
    );
    anyhow::ensure!(
        (1..=12).contains(&count),
        "Keep the diagnostic brief: 1..12 frames"
    );
    let mut encoder = create_encoder(
        width,
        height,
        fps,
        Quality::HighQuality,
        EncoderPreference::Hardware,
        true,
        50_000_000,
    )?;
    encoder.prepare()?;
    encoder.force_keyframe();
    let mut pixels = vec![0; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let p = ((y * width + x) * 4) as usize;
            pixels[p..p + 4].copy_from_slice(&[
                (x % 256) as u8,
                (y % 256) as u8,
                ((x + y) % 256) as u8,
                255,
            ]);
        }
    }
    let mut good = 0;
    let mut empty = 0;
    let mut errors = 0;
    let start = Instant::now();
    for n in 0..count {
        for y in 0..height.min(60) {
            for x in 0..width.min(200) {
                let p = ((y * width + x) * 4) as usize;
                pixels[p] = (n * 3) as u8;
            }
        }
        let frame = Instant::now();
        let mut pending_polls = 0u32;
        let result = loop {
            match encoder.encode_bgra_444(&pixels, width, height, width as usize * 4) {
                Err(error)
                    if error.downcast_ref::<Avc444EncodeError>()
                        == Some(&Avc444EncodeError::Pending) =>
                {
                    anyhow::ensure!(
                        frame.elapsed() < std::time::Duration::from_secs(2),
                        "Synthetic native callback deadline elapsed"
                    );
                    pending_polls += 1;
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                result => break result,
            }
        };
        print!("pending_polls={pending_polls} ");
        match result {
            Ok(pair) if !pair.main_view.data.is_empty() && !pair.aux_view.data.is_empty() => {
                good += 1;
                println!(
                    "frame={n} ms={:.3} main={} aux={} main_idr={} aux_idr={}",
                    frame.elapsed().as_secs_f64() * 1000.0,
                    pair.main_view.data.len(),
                    pair.aux_view.data.len(),
                    pair.main_view.is_keyframe,
                    pair.aux_view.is_keyframe
                );
            }
            Ok(_) => {
                empty += 1;
                println!(
                    "frame={n} ms={:.3} empty",
                    frame.elapsed().as_secs_f64() * 1000.0
                );
            }
            Err(e) => {
                errors += 1;
                println!(
                    "frame={n} ms={:.3} error={e:#}",
                    frame.elapsed().as_secs_f64() * 1000.0
                );
            }
        }
    }
    println!("backend={} size={width}x{height} fps={fps} frames={count} good={good} empty={empty} errors={errors} total_ms={:.3}",encoder.backend_name(),start.elapsed().as_secs_f64()*1000.0);
    Ok(())
}
