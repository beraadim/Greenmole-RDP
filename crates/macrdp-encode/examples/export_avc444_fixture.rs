//! Export synthetic AVC444 pairs, decoded planes, and expected source pixels.
//! No screen capture, desktop access, or input is involved.
//! cargo run -p macrdp-encode --example export_avc444_fixture -- <directory>

use std::{fmt::Write, fs, path::PathBuf};

use anyhow::{Context, Result};
use macrdp_encode::{create_encoder, EncoderPreference, Quality};
use openh264::{decoder::Decoder, formats::YUVSource};

fn pixels(width: u32, height: u32, frame: u32) -> Vec<u8> {
    let mut pixels = vec![0; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let color = if (96..224).contains(&x) && (96..144).contains(&y) {
                // Fine chroma details expose the information lost in AVC420.
                if y < 120 {
                    if x % 2 == 0 {
                        [16, 16, 220, 255]
                    } else {
                        [220, 16, 16, 255]
                    }
                } else if y % 2 == 0 {
                    [16, 220, 16, 255]
                } else {
                    [220, 16, 220, 255]
                }
            } else if x >= 12 + frame * 15 && x < 30 + frame * 15 && (30..80).contains(&y) {
                [255, 255, 255, 255]
            } else if x < width / 2 && y < height / 2 {
                [16, 16, 220, 255]
            } else if y < height / 2 {
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
    pixels
}

fn copy_i420(source: &impl YUVSource) -> Vec<u8> {
    let (w, h) = source.dimensions();
    let (ys, us, vs) = source.strides();
    let mut bytes = Vec::with_capacity(w * h * 3 / 2);
    for (plane, stride, width, height) in [
        (source.y(), ys, w, h),
        (source.u(), us, w / 2, h / 2),
        (source.v(), vs, w / 2, h / 2),
    ] {
        for row in 0..height {
            bytes.extend_from_slice(&plane[row * stride..row * stride + width]);
        }
    }
    bytes
}

fn main() -> Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let output = PathBuf::from(arguments.next().context("Pass an output directory")?);
    anyhow::ensure!(
        arguments.next().is_none(),
        "Only one output directory is accepted"
    );
    fs::create_dir_all(&output)?;
    let mut manifest = String::from("{\"synthetic\":true,\"codec\":\"avc444\",\"version\":1,\"singleDecoder\":true,\"pattern\":{\"left\":96,\"top\":96,\"right\":224,\"bottom\":144},\"sequences\":[");
    let mut first = true;
    for preference in [EncoderPreference::Software, EncoderPreference::Hardware] {
        for (width, height) in [(320u32, 180u32), (352, 208), (318, 178)] {
            let mut encoder = create_encoder(
                width,
                height,
                30.0,
                Quality::HighQuality,
                preference,
                true,
                12_000_000,
            )?;
            encoder.prepare()?;
            encoder.force_keyframe();
            let backend = encoder.backend_name();
            let name = format!("{backend}_{width}x{height}");
            if !first {
                manifest.push(',');
            }
            first = false;
            write!(manifest, "{{\"name\":\"{name}\",\"backend\":\"{backend}\",\"width\":{width},\"height\":{height},\"frames\":[")?;
            let mut decoder = Decoder::new()?;
            for frame in 0u32..6 {
                if frame == 4 {
                    encoder.force_keyframe();
                }
                let pixels = pixels(width, height, frame);
                let encoded =
                    encoder.encode_bgra_444(&pixels, width, height, width as usize * 4)?;
                anyhow::ensure!(
                    !encoded.main_view.data.is_empty() && !encoded.aux_view.data.is_empty(),
                    "Incomplete synthetic pair"
                );
                if frame == 0 || frame == 4 {
                    anyhow::ensure!(
                        encoded.main_view.is_keyframe && encoded.aux_view.is_keyframe,
                        "Requested pair IDR was not produced"
                    );
                }
                let base = format!("{name}_{frame}");
                let main_file = format!("{base}.main.h264");
                let aux_file = format!("{base}.aux.h264");
                let source_file = format!("{base}.bgra");
                fs::write(output.join(&main_file), &encoded.main_view.data)?;
                fs::write(output.join(&aux_file), &encoded.aux_view.data)?;
                fs::write(output.join(&source_file), &pixels)?;
                let mut decoded_files = Vec::new();
                for (kind, data) in [
                    ("main", &encoded.main_view.data),
                    ("aux", &encoded.aux_view.data),
                ] {
                    let decoded = decoder
                        .decode(data)?
                        .context("Pair must decode in main/aux order immediately")?;
                    let file = format!("{base}.{kind}.decoded.i420");
                    fs::write(output.join(&file), copy_i420(&decoded))?;
                    decoded_files.push(file);
                }
                let ew = encoded.main_view.width;
                let eh = encoded.main_view.height;
                let n = (ew * eh) as usize;
                let (mut y, mut u, mut v) = (vec![0; n], vec![0; n], vec![0; n]);
                macrdp_encode::yuv444_split::bgra_to_yuv444_padded(
                    &pixels,
                    width,
                    height,
                    width as usize * 4,
                    ew,
                    eh,
                    &mut y,
                    &mut u,
                    &mut v,
                )?;
                y.extend_from_slice(&u);
                y.extend_from_slice(&v);
                let yuv_file = format!("{base}.source.yuv444");
                fs::write(output.join(&yuv_file), y)?;
                if frame != 0 {
                    manifest.push(',');
                }
                write!(manifest, "{{\"mainFile\":\"{main_file}\",\"auxFile\":\"{aux_file}\",\"sourceFile\":\"{source_file}\",\"sourceYuvFile\":\"{yuv_file}\",\"mainDecodedFile\":\"{}\",\"auxDecodedFile\":\"{}\",\"timestamp\":{},\"keyframe\":{},\"mainKeyframe\":{},\"auxKeyframe\":{},\"encodedWidth\":{ew},\"encodedHeight\":{eh}}}", decoded_files[0], decoded_files[1], u64::from(frame)*33_333, encoded.main_view.is_keyframe && encoded.aux_view.is_keyframe, encoded.main_view.is_keyframe, encoded.aux_view.is_keyframe)?;
            }
            manifest.push_str("]}");
        }
    }
    manifest.push_str("]}\n");
    fs::write(output.join("fixture.json"), manifest)?;
    let mut probe = create_encoder(
        16,
        16,
        30.0,
        Quality::HighQuality,
        EncoderPreference::Software,
        false,
        100_000,
    )?;
    probe.force_keyframe();
    let neutral = probe.encode_bgra(&[128; 16 * 16 * 4], 16, 16, 64)?;
    fs::write(output.join("probe_16x16.h264"), neutral.data)?;
    println!("Synthetic AVC444 fixtures written to {}", output.display());
    Ok(())
}
