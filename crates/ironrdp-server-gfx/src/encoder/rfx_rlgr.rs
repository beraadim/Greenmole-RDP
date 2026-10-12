//! Bounded RemoteFX RLGR writer (MS-RDPRFX 3.1.8.1.7.3).
//!
//! ironrdp-graphics 0.7.0 assumes unlimited output space and uses the wrong
//! RLGR1 GR zero-symbol adaptation (4 instead of 3), corrupting sparse tiles.
//! Counting and writing share the state machine here. The correction matches
//! FreeRDP's RLGR encoder and the protocol's UQ_GR=3 (UP_GR=4 is for RL runs).
//! https://github.com/FreeRDP/FreeRDP/blob/master/libfreerdp/codec/rfx_rlgr.c
use ironrdp_pdu::codecs::rfx::EntropyAlgorithm;

pub(super) fn encode(mode: EntropyAlgorithm, input: &[i16], output: &mut Vec<u8>) {
    let needed = process(mode, input, None);
    output.resize(needed, 0);
    output.fill(0);
    let written = process(mode, input, Some(output));
    debug_assert_eq!(written, needed);
}

fn process(mode: EntropyAlgorithm, input: &[i16], output: Option<&mut [u8]>) -> usize {
    let mut bits = BitWriter {
        output,
        position: 0,
    };
    let mut kp = 8u32;
    let mut krp = 8u32;
    let mut input = input.iter().copied().peekable();
    while input.peek().is_some() {
        let mut k = kp >> 3;
        if k != 0 {
            let mut zeros = 0u32;
            while input.peek() == Some(&0) {
                zeros += 1;
                input.next();
            }
            while zeros >= (1 << k) {
                bits.repeat(1, false);
                zeros -= 1 << k;
                kp = (kp + 4).min(80);
                k = kp >> 3;
            }
            bits.repeat(1, true);
            bits.value(k as usize, zeros);
            if let Some(value) = input.next() {
                bits.repeat(1, value < 0);
                golomb_rice(&mut bits, &mut krp, u32::from(value.unsigned_abs()) - 1);
            }
            kp = kp.saturating_sub(6);
        } else {
            let first = magnitude_sign(input.next().expect("peeked a sample"));
            match mode {
                EntropyAlgorithm::Rlgr1 => {
                    golomb_rice(&mut bits, &mut krp, first);
                    // UQ_GR=3 in GR mode; UP_GR=4 is only for an RL zero run.
                    kp = if first == 0 {
                        (kp + 3).min(80)
                    } else {
                        kp.saturating_sub(3)
                    };
                }
                EntropyAlgorithm::Rlgr3 => {
                    // A missing partner is a zero coefficient, not magnitude 1.
                    let second = input.next().map(magnitude_sign).unwrap_or(0);
                    let sum = first + second;
                    golomb_rice(&mut bits, &mut krp, sum);
                    bits.value((32 - sum.leading_zeros()) as usize, first);
                    if first != 0 && second != 0 {
                        kp = kp.saturating_sub(6);
                    } else if first == 0 && second == 0 {
                        kp = (kp + 6).min(80);
                    }
                }
            }
        }
    }
    bits.position.div_ceil(8)
}

fn magnitude_sign(value: i16) -> u32 {
    2 * u32::from(value.unsigned_abs()) - u32::from(value < 0)
}

fn golomb_rice(bits: &mut BitWriter<'_>, krp: &mut u32, value: u32) {
    let kr = *krp >> 3;
    let unary = value >> kr;
    bits.repeat(unary as usize, true);
    bits.repeat(1, false);
    bits.value(kr as usize, value & ((1 << kr) - 1));
    if unary == 0 {
        *krp = krp.saturating_sub(2);
    } else if unary > 1 {
        *krp = (*krp + unary).min(80);
    }
}

struct BitWriter<'a> {
    output: Option<&'a mut [u8]>,
    position: usize,
}

impl BitWriter<'_> {
    fn repeat(&mut self, mut count: usize, value: bool) {
        // Output starts zeroed, so zero runs and the sizing pass only advance.
        if !value || self.output.is_none() {
            self.position += count;
            return;
        }
        if self.position % 8 != 0 {
            let head = count.min(8 - self.position % 8);
            self.value(head, (1 << head) - 1);
            count -= head;
        }
        let bytes = count / 8;
        if let Some(output) = self.output.as_deref_mut() {
            output[self.position / 8..self.position / 8 + bytes].fill(255);
        }
        self.position += bytes * 8;
        let tail = count % 8;
        self.value(tail, (1 << tail) - 1);
    }

    fn value(&mut self, mut count: usize, value: u32) {
        if let Some(output) = self.output.as_deref_mut() {
            while count != 0 {
                let take = count.min(8 - self.position % 8);
                count -= take;
                let part = ((value >> count) & ((1 << take) - 1)) as u8;
                output[self.position / 8] |= part << (8 - self.position % 8 - take);
                self.position += take;
            }
        } else {
            self.position += count;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coefficients_roundtrip_in_both_modes_with_sparse_runs_and_odd_pairs() {
        let mut seed = 0x1234_5678u32;
        for mode in [EntropyAlgorithm::Rlgr1, EntropyAlgorithm::Rlgr3] {
            for length in [1, 3, 31, 4096] {
                for case in 0..8 {
                    let mut input = vec![0i16; length];
                    for (index, value) in input.iter_mut().enumerate() {
                        seed ^= seed << 13;
                        seed ^= seed >> 17;
                        seed ^= seed << 5;
                        *value = match case {
                            0 => 0,
                            1 => 1,
                            2 => {
                                if index % 2 == 0 {
                                    -32767
                                } else {
                                    32767
                                }
                            }
                            3 => {
                                if index % 127 == 0 {
                                    32767
                                } else {
                                    0
                                }
                            }
                            4 => {
                                if index % 3 == 0 {
                                    1
                                } else {
                                    0
                                }
                            }
                            _ => ((seed as i16) >> (case - 4)).max(-32767),
                        };
                    }
                    let mut output = vec![255; 3];
                    encode(mode, &input, &mut output);
                    assert_eq!(output.len(), process(mode, &input, None));
                    let mut decoded = vec![0; length];
                    ironrdp_graphics::rlgr::decode(mode, &output, &mut decoded).unwrap();
                    assert_eq!(
                        decoded, input,
                        "mode={mode:?}, length={length}, case={case}"
                    );
                }
            }
        }
    }

    #[test]
    fn minimum_i16_does_not_overflow_magnitude_or_output_sizing() {
        for mode in [EntropyAlgorithm::Rlgr1, EntropyAlgorithm::Rlgr3] {
            let mut output = Vec::new();
            encode(mode, &[i16::MIN; 4096], &mut output);
            assert_eq!(output.len(), process(mode, &[i16::MIN; 4096], None));
        }
    }
}
