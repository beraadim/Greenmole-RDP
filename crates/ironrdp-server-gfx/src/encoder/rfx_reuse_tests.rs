use super::*;
use crate::encoder::quality_tests::fixture;

fn encoder() -> RfxEncoder {
    RfxEncoder::new(
        EntropyBits::Rlgr3,
        crate::BitmapQuality::HighQuality,
        OperatingMode::IMAGE_MODE,
    )
}

fn desktop() -> Option<DesktopSize> {
    Some(DesktopSize {
        width: 1920,
        height: 1080,
    })
}

#[test]
fn narrow_noisy_images_compress_once_and_reuse_component_and_output_buffers() {
    let bitmap = fixture(1, 257, true);
    let mut encoder = encoder();
    let mut output = Vec::new();
    let len = encoder
        .encode(&bitmap, &mut output, desktop(), usize::MAX)
        .unwrap()
        .unwrap();
    assert!(len > usize::from(bitmap.width.get()) * usize::from(bitmap.height.get()) * 4 + 256);
    assert_eq!(encoder.compressed_tiles, 5);
    let expected = output.clone();
    let output_address = output.as_ptr();
    let addresses: Vec<_> = encoder
        .data
        .0
        .iter()
        .map(|tile| (tile.y.as_ptr(), tile.cb.as_ptr(), tile.cr.as_ptr()))
        .collect();
    encoder
        .encode(&bitmap, &mut output, desktop(), usize::MAX)
        .unwrap()
        .unwrap();
    assert_eq!(encoder.compressed_tiles, 10);
    assert_eq!(output, expected);
    assert_eq!(output.as_ptr(), output_address);
    assert_eq!(
        addresses,
        encoder
            .data
            .0
            .iter()
            .map(|tile| { (tile.y.as_ptr(), tile.cb.as_ptr(), tile.cr.as_ptr()) })
            .collect::<Vec<_>>()
    );
}

#[test]
fn rejected_candidates_do_not_serialize_or_advance_video_frame_index() {
    let bitmap = fixture(65, 65, true);
    let mut encoder = encoder();
    encoder.mode = OperatingMode::empty();
    let mut output = vec![9; 32];
    assert!(encoder
        .encode(&bitmap, &mut output, desktop(), 32)
        .unwrap()
        .is_none());
    assert_eq!(output, vec![9; 32]);
    assert_eq!(encoder.frame_index, 0);
    encoder
        .encode(&bitmap, &mut output, desktop(), usize::MAX)
        .unwrap()
        .unwrap();
    assert_eq!(
        encoder.frame_index, 0,
        "preparation alone cannot commit a frame"
    );
    encoder.commit_frame();
    assert_eq!(encoder.frame_index, 1);
}

#[test]
fn scratch_cache_discards_excess_capacity_and_preserves_reusable_prefix() {
    let mut data = UpdateEncoderData(vec![TileData::default(); 3]);
    data.0[0].y = Vec::with_capacity(1024);
    data.0[1].y = Vec::with_capacity(MAX_CACHED_BYTES);
    data.0[2].cr = Vec::with_capacity(1024);
    let retained = data.0[0].y.as_ptr();
    data.trim_cache();
    assert!(data.retained_bytes() <= MAX_CACHED_BYTES);
    assert_eq!(data.0.len(), 1);
    assert_eq!(data.0[0].y.as_ptr(), retained);
}

/// Run with `cargo test -p ironrdp-server --release rfx_encode_once_benchmark
/// -- --ignored --nocapture`. This compares the previous grow-and-recompress
/// strategy with warm bounded scratch on identical deterministic source pixels.
/// It is an observation benchmark, not a timing-dependent correctness test.
#[test]
#[ignore = "manual release-mode CPU benchmark"]
fn rfx_encode_once_benchmark() {
    use std::time::Instant;
    for (width, height) in [(1, 257), (513, 257)] {
        let bitmap = fixture(width, height, true);
        let iterations = 20;
        // Initialize shared worker pools before either timed path. Discard the
        // encoder and its buffers so baseline retries still start cold.
        encoder()
            .encode(&bitmap, &mut Vec::new(), desktop(), usize::MAX)
            .unwrap();
        let mut baseline_tiles = 0;
        let started = Instant::now();
        let mut expected = Vec::new();
        for _ in 0..iterations {
            let mut limit = usize::from(width) * usize::from(height) * 4 + 256;
            loop {
                // The previous handler allocated component storage afresh on
                // every output-size retry and for every frame.
                let mut cold = encoder();
                let result = cold
                    .encode(&bitmap, &mut expected, desktop(), limit)
                    .unwrap();
                baseline_tiles += cold.compressed_tiles;
                if result.is_some() {
                    break;
                }
                limit *= 2;
            }
        }
        let baseline = started.elapsed();
        let mut warm = encoder();
        let mut actual = Vec::new();
        warm.encode(&bitmap, &mut actual, desktop(), usize::MAX)
            .unwrap();
        warm.compressed_tiles = 0;
        let started = Instant::now();
        for _ in 0..iterations {
            warm.encode(&bitmap, &mut actual, desktop(), usize::MAX)
                .unwrap();
            std::hint::black_box(&actual);
        }
        let optimized = started.elapsed();
        assert_eq!(actual, expected);
        assert!(warm.data.retained_bytes() <= MAX_CACHED_BYTES);
        eprintln!("RFX {width}x{height} noise, {iterations} frames: baseline={baseline:?}, encode_once={optimized:?}, compressed_tiles={baseline_tiles}/{}, output_bytes={}, cache_bytes={}", warm.compressed_tiles, actual.len(), warm.data.retained_bytes());
    }
}
