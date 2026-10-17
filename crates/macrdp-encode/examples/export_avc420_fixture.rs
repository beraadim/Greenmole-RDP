//! Export real AVC420 access units without reading a screen or posting input.
//!
//! cargo run -p macrdp-encode --example export_avc420_fixture -- <output-directory>
//! The manifest contains synthetic-only frames for browser decoder integration
//! tests. Every sequence has an initial IDR, delta frames, and a forced IDR.

use std::{fmt::Write, fs, path::PathBuf};

use anyhow::{Context, Result};
use macrdp_encode::{create_encoder, EncoderPreference, Quality};

fn main() -> Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let output = PathBuf::from(arguments.next().context("Pass an output directory")?);
    anyhow::ensure!(
        arguments.next().is_none(),
        "Only one output directory is accepted"
    );
    fs::create_dir_all(&output)?;
    let mut manifest = String::from("{\"synthetic\":true,\"sequences\":[");
    let mut first_sequence = true;
    for preference in [EncoderPreference::Software, EncoderPreference::Hardware] {
        for (width, height) in [(320u32, 180u32), (352, 208)] {
            let mut encoder = create_encoder(
                width,
                height,
                30.0,
                Quality::HighQuality,
                preference,
                false,
                3_000_000,
            )?;
            encoder.prepare()?;
            encoder.force_keyframe();
            let backend = encoder.backend_name();
            let name = format!("{backend}_{width}x{height}");
            if !first_sequence {
                manifest.push(',');
            }
            first_sequence = false;
            write!(manifest,
                "{{\"name\":\"{name}\",\"backend\":\"{backend}\",\"width\":{width},\"height\":{height},\"frames\":["
            )?;
            for frame in 0u32..6 {
                if frame == 4 {
                    encoder.force_keyframe();
                }
                // Flat color blocks make chroma/cropping errors visible. The
                // advancing white marker ensures each delta changes pixels.
                let mut pixels = vec![0u8; (width * height * 4) as usize];
                for y in 0..height {
                    for x in 0..width {
                        let marker =
                            x >= 12 + frame * 15 && x < 30 + frame * 15 && y >= 30 && y < 80;
                        let color = if marker {
                            [255, 255, 255, 255]
                        } else if x < width / 2 && y < height / 2 {
                            [16, 16, 220, 255]
                        } else if x >= width / 2 && y < height / 2 {
                            [16, 220, 16, 255]
                        } else if x < width / 2 {
                            [220, 16, 16, 255]
                        } else {
                            [28, 28, 28, 255]
                        };
                        let offset = ((y * width + x) * 4) as usize;
                        pixels[offset..offset + 4].copy_from_slice(&color);
                    }
                }
                let encoded = encoder.encode_bgra(&pixels, width, height, (width * 4) as usize)?;
                anyhow::ensure!(
                    !encoded.data.is_empty(),
                    "Synthetic encoder returned no data"
                );
                if frame == 0 || frame == 4 {
                    anyhow::ensure!(encoded.is_keyframe, "Requested IDR was not produced");
                }
                let file = format!("{name}_{frame}.h264");
                fs::write(output.join(&file), &encoded.data)?;
                if frame != 0 {
                    manifest.push(',');
                }
                write!(manifest,
                    "{{\"file\":\"{file}\",\"timestamp\":{},\"keyframe\":{},\"encodedWidth\":{},\"encodedHeight\":{}}}",
                    u64::from(frame) * 33_333, encoded.is_keyframe, encoded.width, encoded.height
                )?;
            }
            manifest.push_str("]}");
        }
    }
    manifest.push_str("]}\n");
    fs::write(output.join("fixture.json"), manifest)?;
    println!("Synthetic AVC420 fixture written to {}", output.display());
    Ok(())
}
