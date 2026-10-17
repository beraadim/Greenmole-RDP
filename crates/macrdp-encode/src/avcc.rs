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

/// Every IDR is a decoder entry point, including after a browser reconnect or
/// resolution change. Keep its format description in-band instead of relying
/// on parameter sets from a previous access unit or a native decoder cache.
pub(crate) fn self_contained_idr<'a>(
    slices: &[u8],
    parameter_sets: impl IntoIterator<Item = &'a [u8]>,
) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut has_sps = false;
    let mut has_pps = false;
    for parameter in parameter_sets {
        let header = parameter
            .first()
            .ok_or_else(|| anyhow::anyhow!("Empty H.264 parameter set"))?;
        anyhow::ensure!(header & 0x80 == 0, "Invalid H.264 parameter-set header");
        has_sps |= header & 0x1f == 7;
        has_pps |= header & 0x1f == 8;
        append_nal(&mut output, parameter)?;
    }
    anyhow::ensure!(has_sps && has_pps, "H.264 IDR is missing SPS or PPS");
    anyhow::ensure!(
        output.len().saturating_add(slices.len()) <= MAX_ENCODED_BYTES,
        "Encoded frame plus parameter sets exceeds output limit"
    );
    output.try_reserve(slices.len())?;
    output.extend_from_slice(slices);
    Ok(output)
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

    #[test]
    fn idr_carries_parameter_sets_before_its_slices_for_new_decoders() {
        let sps = [0x67, 0x42, 0xc0, 0x1f];
        let pps = [0x68, 0xce, 0x3c, 0x80];
        let slices = [0, 0, 0, 1, 0x65, 0xb8];
        let output = self_contained_idr(&slices, [&sps[..], &pps[..]]).unwrap();
        assert_eq!(
            output,
            [
                0, 0, 0, 1, 0x67, 0x42, 0xc0, 0x1f, 0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80, 0, 0, 0, 1,
                0x65, 0xb8
            ]
        );
    }

    #[test]
    fn incomplete_or_invalid_idr_format_is_rejected_instead_of_poisoning_decoder() {
        let sps = [0x67, 0x42, 0xc0, 0x1f];
        let pps = [0x68, 0xce, 0x3c, 0x80];
        let slices = [0, 0, 0, 1, 0x65, 0xb8];
        for parameters in [vec![], vec![&sps[..]], vec![&pps[..]], vec![&[][..]]] {
            assert!(self_contained_idr(&slices, parameters).is_err());
        }
        assert!(self_contained_idr(&slices, [&sps[..], &pps[..], &[0xe8][..]]).is_err());
    }
}
