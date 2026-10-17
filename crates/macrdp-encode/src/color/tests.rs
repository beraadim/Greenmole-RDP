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
