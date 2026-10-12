//! Checked conversion of length-prefixed H.264 output into Annex B.
use anyhow::{Context, Result};

pub(crate) const MAX_ENCODED_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn append_nal(output: &mut Vec<u8>, nal: &[u8]) -> Result<()> {
    let additional = nal
        .len()
        .checked_add(4)
        .ok_or_else(|| anyhow::anyhow!("NAL size overflow"))?;
    anyhow::ensure!(
        output.len().saturating_add(additional) <= MAX_ENCODED_BYTES,
        "Encoded frame exceeds output limit"
    );
    output
        .try_reserve(additional)
        .context("Cannot allocate encoded frame")?;
    output.extend_from_slice(&[0, 0, 0, 1]);
    output.extend_from_slice(nal);
    Ok(())
}

pub(crate) fn convert(data: &[u8], length_size: usize) -> Result<(Vec<u8>, bool)> {
    anyhow::ensure!(
        matches!(length_size, 1 | 2 | 4),
        "Invalid AVCC NAL length width"
    );
    anyhow::ensure!(
        data.len() <= MAX_ENCODED_BYTES,
        "Encoded frame exceeds input limit"
    );
    let mut remaining = data;
    let mut output = Vec::new();
    let mut keyframe = false;
    while !remaining.is_empty() {
        let header = remaining
            .get(..length_size)
            .ok_or_else(|| anyhow::anyhow!("Truncated AVCC length"))?;
        let length = header
            .iter()
            .fold(0usize, |length, byte| (length << 8) | usize::from(*byte));
        remaining = &remaining[length_size..];
        anyhow::ensure!(length > 0, "Empty AVCC NAL");
        let nal = remaining
            .get(..length)
            .ok_or_else(|| anyhow::anyhow!("Truncated AVCC NAL"))?;
        remaining = &remaining[length..];
        let kind = nal[0] & 0x1f;
        keyframe |= kind == 5;
        // Keep slices and parameter sets. VT recovery SEI can reset the
        // Windows decoder's reference pictures, so ancillary NALs are omitted.
        if matches!(kind, 1 | 5 | 7 | 8) {
            append_nal(&mut output, nal)?;
        }
    }
    Ok((output, keyframe))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_supported_length_widths_preserve_slices_and_omit_ancillary_nals() {
        for length_size in [1, 2, 4] {
            let mut avcc = Vec::new();
            for nal in [&[0x06, 0x10][..], &[0x65, 0x20][..], &[0x41, 0x30][..]] {
                avcc.extend_from_slice(&(nal.len() as u32).to_be_bytes()[4 - length_size..]);
                avcc.extend_from_slice(nal);
            }
            let (output, keyframe) = convert(&avcc, length_size).unwrap();
            assert_eq!(output, [0, 0, 0, 1, 0x65, 0x20, 0, 0, 0, 1, 0x41, 0x30]);
            assert!(keyframe);
        }
    }

    #[test]
    fn malformed_nals_fail_without_emitting_a_partial_frame() {
        for (data, size) in [
            (vec![0], 1),
            (vec![1], 1),
            (vec![0], 2),
            (vec![0, 0, 0], 4),
            (vec![255, 255, 255, 255], 4),
            (vec![1, 0x65, 2, 0x41], 1),
            (vec![1, 0x65], 3),
        ] {
            assert!(convert(&data, size).is_err());
        }
        assert!(!convert(&[1, 0x41], 1).unwrap().1);
    }
}
