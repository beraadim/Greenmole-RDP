use crate::VideoEncoder;
use openh264::decoder::Decoder;
use openh264::formats::YUVSource;

/// Decode the actual native encoder output, then apply the independent
/// normative RDP inverse. Gray-only checks cannot distinguish 601 from 709
/// or expose compressed/expanded luma ranges at black and white.
pub(crate) fn assert_encoded_color_fidelity(
    encoder: &mut dyn VideoEncoder,
    width: u32,
    height: u32,
) {
    let colors = [
        [220u8, 16, 16],
        [16, 220, 16],
        [16, 16, 220],
        [0, 0, 0],
        [255, 255, 255],
        [128, 128, 128],
        [180, 70, 140],
        [235, 235, 235],
    ];
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    for y in 0..height as usize {
        for x in 0..width as usize {
            let [r, g, b] = colors[(x / 16).min(colors.len() - 1)];
            let offset = (y * width as usize + x) * 4;
            pixels[offset..offset + 4].copy_from_slice(&[b, g, r, 255]);
        }
    }
    encoder.prepare().unwrap();
    encoder.force_keyframe();
    let encoded = encoder
        .encode_bgra(&pixels, width, height, width as usize * 4)
        .unwrap();
    assert!(encoded.is_keyframe);
    let mut decoder = Decoder::new().unwrap();
    let decoded = decoder
        .decode(&encoded.data)
        .unwrap()
        .expect("IDR must decode immediately");
    let (y_stride, u_stride, v_stride) = decoded.strides();
    for (index, expected) in colors.iter().enumerate() {
        let x = index * 16 + 8;
        let row = height as usize / 2;
        let y = f64::from(decoded.y()[row * y_stride + x]);
        let u = f64::from(decoded.u()[(row / 2) * u_stride + x / 2]) - 128.0;
        let v = f64::from(decoded.v()[(row / 2) * v_stride + x / 2]) - 128.0;
        let clamp = |value: f64| value.round().clamp(0.0, 255.0) as u8;
        let rgb = [
            clamp(y + 1.5748 * v),
            clamp(y - 0.187324 * u - 0.468124 * v),
            clamp(y + 1.8556 * u),
        ];
        for channel in 0..3 {
            assert!(
                rgb[channel].abs_diff(expected[channel]) <= 5,
                "{} {width}x{height} color {index}: decoded {rgb:?}, expected {expected:?}",
                encoder.backend_name()
            );
        }
    }
}

#[test]
fn scalar_709_round_trip_preserves_chromatic_pixels_and_full_luma_endpoints() {
    for [r, g, b] in [
        [220u8, 16, 16],
        [16, 220, 16],
        [16, 16, 220],
        [0, 0, 0],
        [255, 255, 255],
        [128, 128, 128],
        [180, 70, 140],
    ] {
        let y = f64::from(super::luma(i32::from(r), i32::from(g), i32::from(b)));
        let (u, v) = super::chroma(i32::from(r), i32::from(g), i32::from(b));
        let (u, v) = (f64::from(u) - 128.0, f64::from(v) - 128.0);
        let rgb = [
            y + 1.5748 * v,
            y - 0.187324 * u - 0.468124 * v,
            y + 1.8556 * u,
        ];
        for (actual, expected) in rgb.into_iter().zip([r, g, b]) {
            assert!((actual - f64::from(expected)).abs() < 2.0);
        }
    }
    assert_eq!(super::luma(0, 0, 0), 0);
    assert_eq!(super::luma(255, 255, 255), 255);
}

/// Decode the ordered AVC444 bitstream with one real decoder, then use the
/// normative v1 inverse (MS-RDPEGFX 3.3.8.3.2), independent of the encoder split.
/// One-pixel colored lines make an AVC420-only result fail decisively.
pub(crate) fn assert_avc444_fidelity(encoder: &mut dyn VideoEncoder, width: u32, height: u32) {
    fn planes(source: &impl YUVSource) -> [Vec<u8>; 3] {
        let (w, h) = source.dimensions();
        let (ys, us, vs) = source.strides();
        std::array::from_fn(|index| {
            let (data, stride, width, height) = match index {
                0 => (source.y(), ys, w, h),
                1 => (source.u(), us, w / 2, h / 2),
                _ => (source.v(), vs, w / 2, h / 2),
            };
            (0..height)
                .flat_map(|row| data[row * stride..row * stride + width].iter().copied())
                .collect()
        })
    }
    encoder.prepare().unwrap();
    let mut decoder = Decoder::new().unwrap();
    for frame in 0..6 {
        if frame == 0 || frame == 4 {
            encoder.force_keyframe();
        }
        let mut pixels = vec![0; (width * height * 4) as usize];
        for y in 0..height {
            for x in 0..width {
                let color = if (x + frame) % 2 == 0 {
                    [16, 16, 220, 255]
                } else {
                    [220, 16, 16, 255]
                };
                let offset = ((y * width + x) * 4) as usize;
                pixels[offset..offset + 4].copy_from_slice(&color);
            }
        }
        let pair = encoder
            .encode_bgra_444(&pixels, width, height, width as usize * 4)
            .unwrap();
        assert!(!pair.main_view.data.is_empty() && !pair.aux_view.data.is_empty());
        if frame == 0 || frame == 4 {
            assert!(pair.main_view.is_keyframe && pair.aux_view.is_keyframe);
        } else {
            assert!(
                !pair.main_view.is_keyframe && !pair.aux_view.is_keyframe,
                "Alternating AVC444 representations must retain the H.264 P-frame chain"
            );
        }
        let main = planes(
            &decoder
                .decode(&pair.main_view.data)
                .unwrap()
                .expect("Main view must decode immediately"),
        );
        let aux = planes(
            &decoder
                .decode(&pair.aux_view.data)
                .unwrap()
                .expect("Auxiliary must decode with the same decoder"),
        );
        let ew = pair.main_view.width as usize;
        let mut full_error = 0.0;
        let mut subsampled_error = 0.0;
        for y in 0..height as usize {
            for x in 0..width as usize {
                let uv = |plane: usize, restore: bool| {
                    let average = i32::from(main[plane][(y / 2) * (ew / 2) + x / 2]);
                    if !restore {
                        return average as f64 - 128.0;
                    }
                    let packed_row = (y / 2) / 8 * 16 + (y / 2) % 8 + (plane - 1) * 8;
                    let value = if y % 2 != 0 {
                        i32::from(aux[0][packed_row * ew + x])
                    } else if x % 2 != 0 {
                        i32::from(aux[plane][(y / 2) * (ew / 2) + x / 2])
                    } else {
                        4 * average
                            - i32::from(aux[plane][(y / 2) * (ew / 2) + x / 2])
                            - i32::from(aux[0][packed_row * ew + x])
                            - i32::from(aux[0][packed_row * ew + x + 1])
                    };
                    value.clamp(0, 255) as f64 - 128.0
                };
                for (restore, error) in [(false, &mut subsampled_error), (true, &mut full_error)] {
                    let yy = f64::from(main[0][y * ew + x]);
                    let (u, v) = (uv(1, restore), uv(2, restore));
                    let rgb = [
                        yy + 1.5748 * v,
                        yy - 0.187324 * u - 0.468124 * v,
                        yy + 1.8556 * u,
                    ];
                    let offset = (y * width as usize + x) * 4;
                    let expected = [pixels[offset + 2], pixels[offset + 1], pixels[offset]];
                    *error += rgb
                        .into_iter()
                        .zip(expected)
                        .map(|(a, b)| (a.clamp(0.0, 255.0) - f64::from(b)).abs())
                        .sum::<f64>();
                }
            }
        }
        let mean = full_error / f64::from(width * height * 3);
        assert!(
            mean < 8.0 && full_error < subsampled_error * 0.1,
            "{} {width}x{height} pair{frame}: AVC444 mean error={mean}, AVC420 mean error={}",
            encoder.backend_name(),
            subsampled_error / f64::from(width * height * 3)
        );
    }
}
