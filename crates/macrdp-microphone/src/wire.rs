//! Versioned, bounded Core Audio custom-property messages. No audio is stored
//! in the filesystem or exposed by the driver's readable properties.

pub(crate) const VERSION: i32 = 1;
pub(crate) const MAX_FRAMES: usize = 1024;
pub(crate) const HEADER_BYTES: usize = 24;
pub(crate) const BEGIN: u32 = 1;
pub(crate) const PCM: u32 = 2;
pub(crate) const END: u32 = 3;

pub(crate) fn message(opcode: u32, generation: u64, samples: &[f32]) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(generation != 0, "Invalid microphone session generation");
    anyhow::ensure!(
        matches!(opcode, BEGIN | PCM | END),
        "Invalid microphone operation"
    );
    anyhow::ensure!(
        samples.len() % 2 == 0 && samples.len() / 2 <= MAX_FRAMES,
        "Invalid microphone sample count"
    );
    anyhow::ensure!(
        (opcode == PCM) == !samples.is_empty(),
        "Invalid microphone operation payload"
    );
    anyhow::ensure!(
        samples
            .iter()
            .all(|sample| sample.is_finite() && (-1.0..=1.0).contains(sample)),
        "Invalid microphone sample value"
    );
    let mut bytes = Vec::with_capacity(HEADER_BYTES + samples.len() * 4);
    bytes.extend_from_slice(b"GMM1");
    bytes.extend_from_slice(&opcode.to_le_bytes());
    bytes.extend_from_slice(&generation.to_le_bytes());
    bytes.extend_from_slice(&(samples.len() as u32 / 2).to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_wire_contract_has_explicit_little_endian_fields() {
        let bytes = message(PCM, 0x1020304050607080, &[0.5, -0.25]).unwrap();
        assert_eq!(bytes.len(), HEADER_BYTES + 8);
        assert_eq!(&bytes[..4], b"GMM1");
        assert_eq!(&bytes[4..8], &2_u32.to_le_bytes());
        assert_eq!(&bytes[8..16], &0x1020304050607080_u64.to_le_bytes());
        assert_eq!(&bytes[16..24], &[1, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&bytes[24..28], &0.5_f32.to_le_bytes());
    }

    #[test]
    fn rejects_invalid_frames_before_allocating_or_calling_hal() {
        assert!(message(BEGIN, 0, &[]).is_err());
        assert!(message(PCM, 1, &[]).is_err());
        assert!(message(PCM, 1, &[0.0]).is_err());
        assert!(message(PCM, 1, &vec![0.0; (MAX_FRAMES + 1) * 2]).is_err());
        assert!(message(PCM, 1, &[f32::NAN, 0.0]).is_err());
        assert!(message(PCM, 1, &[1.1, 0.0]).is_err());
        assert!(message(END, 1, &[0.0, 0.0]).is_err());
        assert!(message(99, 1, &[]).is_err());
    }
}
