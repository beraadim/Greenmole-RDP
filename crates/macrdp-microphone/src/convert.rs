//! Packet-sized PCM conversion with a persistent band-limited resampler.
//! Client and HAL sample clocks have separate state for each RDP connection.

use ironrdp_server::MicrophonePcmFormat;
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

pub(crate) struct Converter {
    format: MicrophonePcmFormat,
    frames: usize,
    pending_frames: usize,
    input: Vec<Vec<f32>>,
    output: Vec<Vec<f32>>,
    resampler: Option<SincFixedIn<f32>>,
    interleaved: Vec<f32>,
}

impl Converter {
    pub(crate) fn new(format: MicrophonePcmFormat) -> anyhow::Result<Self> {
        anyhow::ensure!(
            matches!(format.sample_rate, 44_100 | 48_000) && matches!(format.channels, 1 | 2),
            "Unsupported microphone PCM format"
        );
        let frames = format.sample_rate as usize / 100;
        let resampler = if format.sample_rate != 48_000 {
            Some(SincFixedIn::<f32>::new(
                48_000.0 / f64::from(format.sample_rate),
                1.0,
                SincInterpolationParameters {
                    sinc_len: 128,
                    f_cutoff: 0.95,
                    interpolation: SincInterpolationType::Linear,
                    oversampling_factor: 128,
                    window: WindowFunction::BlackmanHarris2,
                },
                frames,
                2,
            )?)
        } else {
            None
        };
        let output = resampler.as_ref().map_or_else(
            || vec![vec![0.0; frames]; 2],
            |resampler| resampler.output_buffer_allocate(true),
        );
        Ok(Self {
            format,
            frames,
            pending_frames: 0,
            input: vec![vec![0.0; frames]; 2],
            output,
            resampler,
            interleaved: Vec::with_capacity(crate::wire::MAX_FRAMES * 2),
        })
    }

    pub(crate) fn reset(&mut self) {
        if let Some(resampler) = &mut self.resampler {
            resampler.reset();
        }
        self.input.iter_mut().for_each(|channel| channel.fill(0.0));
        self.output.iter_mut().for_each(|channel| channel.fill(0.0));
        self.interleaved.clear();
        self.pending_frames = 0;
    }

    pub(crate) fn convert(&mut self, pcm: &[u8]) -> anyhow::Result<&[f32]> {
        let channels = usize::from(self.format.channels);
        anyhow::ensure!(
            !pcm.is_empty()
                && pcm.len() <= self.frames * channels * 2
                && pcm.len() % (channels * 2) == 0,
            "Invalid microphone PCM packet size"
        );
        self.interleaved.clear();
        for sample_bytes in pcm.chunks_exact(channels * 2) {
            let left = f32::from(i16::from_le_bytes([sample_bytes[0], sample_bytes[1]])) / 32768.0;
            let right = if channels == 2 {
                f32::from(i16::from_le_bytes([sample_bytes[2], sample_bytes[3]])) / 32768.0
            } else {
                left
            };
            self.input[0][self.pending_frames] = left;
            self.input[1][self.pending_frames] = right;
            self.pending_frames += 1;
            if self.pending_frames == self.frames {
                self.convert_ready_packet()?;
                self.pending_frames = 0;
            }
        }
        Ok(&self.interleaved)
    }

    fn convert_ready_packet(&mut self) -> anyhow::Result<()> {
        let output_frames = if let Some(resampler) = &mut self.resampler {
            resampler
                .process_into_buffer(&self.input, &mut self.output, None)?
                .1
        } else {
            self.frames
        };
        anyhow::ensure!(
            output_frames <= crate::wire::MAX_FRAMES,
            "Microphone resampler output exceeded limit"
        );
        let source = if self.resampler.is_some() {
            &self.output
        } else {
            &self.input
        };
        for (&left, &right) in source[0].iter().zip(&source[1]).take(output_frames) {
            // A band-limited reconstruction can slightly overshoot full scale.
            self.interleaved.push(left.clamp(-1.0, 1.0));
            self.interleaved.push(right.clamp(-1.0, 1.0));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(sample_rate: u32, channels: u16) -> MicrophonePcmFormat {
        MicrophonePcmFormat {
            sample_rate,
            channels,
        }
    }

    #[test]
    fn native_pcm_conversion_preserves_stereo_and_duplicates_mono() {
        let packet: Vec<u8> = [i16::MIN, i16::MAX]
            .into_iter()
            .flat_map(i16::to_le_bytes)
            .cycle()
            .take(480 * 4)
            .collect();
        let mut converter = Converter::new(format(48_000, 2)).unwrap();
        let samples = converter.convert(&packet).unwrap();
        assert_eq!(samples.len(), 960);
        assert_eq!(&samples[..2], &[-1.0, 32767.0 / 32768.0]);
        let mut converter = Converter::new(format(48_000, 1)).unwrap();
        let mono = vec![128_u8, 0]
            .into_iter()
            .cycle()
            .take(480 * 2)
            .collect::<Vec<_>>();
        assert!(converter
            .convert(&mono)
            .unwrap()
            .iter()
            .all(|sample| *sample == 128.0 / 32768.0));
    }

    #[test]
    fn resampling_preserves_voice_frequency_and_sample_clock() {
        let mut converter = Converter::new(format(44_100, 1)).unwrap();
        let mut output = Vec::new();
        for packet in 0..100 {
            let pcm: Vec<u8> = (0..441)
                .flat_map(|frame| {
                    let time = (packet * 441 + frame) as f64 / 44_100.0;
                    (((time * 1000.0 * std::f64::consts::TAU).sin() * 16_000.0).round() as i16)
                        .to_le_bytes()
                })
                .collect();
            output.extend_from_slice(converter.convert(&pcm).unwrap());
        }
        let frames = output.len() / 2;
        assert!(
            (47_800..=48_000).contains(&frames),
            "unexpected resampler length {frames}"
        );
        let left: Vec<_> = output
            .chunks_exact(2)
            .skip(1000)
            .map(|frame| frame[0])
            .collect();
        let crossings = left
            .windows(2)
            .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
            .count();
        let frequency = crossings as f64 * 48_000.0 / left.len() as f64;
        assert!((frequency - 1000.0).abs() < 2.0);
        assert!(output.chunks_exact(2).all(|frame| frame[0] == frame[1]));
        converter.reset();
        assert!(converter
            .convert(&vec![0; 441 * 2])
            .unwrap()
            .iter()
            .all(|sample| *sample == 0.0));
    }

    #[test]
    fn invalid_format_or_packet_size_is_rejected() {
        assert!(Converter::new(format(96_000, 2)).is_err());
        assert!(Converter::new(format(48_000, 8)).is_err());
        let mut converter = Converter::new(format(48_000, 2)).unwrap();
        assert!(converter.convert(&[]).is_err());
        assert!(converter.convert(&vec![0; 480 * 4 + 1]).is_err());
    }

    #[test]
    fn fragmented_pcm_has_identical_audio_and_clock_and_reset_clears_partials() {
        for rate in [44_100, 48_000] {
            let format = format(rate, 2);
            let frames = rate as usize / 100;
            let pcm: Vec<u8> = (0..frames * 2)
                .flat_map(|sample| ((sample % 30000) as i16).to_le_bytes())
                .collect();
            let mut complete = Converter::new(format).unwrap();
            let mut fragmented = Converter::new(format).unwrap();
            let mut actual = Vec::new();
            for _ in 0..3 {
                let expected = complete.convert(&pcm).unwrap();
                for part in pcm.chunks(28) {
                    actual.extend_from_slice(fragmented.convert(part).unwrap());
                }
                assert_eq!(&actual, expected);
                actual.clear();
            }
            assert!(fragmented.convert(&pcm[..4]).unwrap().is_empty());
            fragmented.reset();
            complete.reset();
            assert_eq!(
                fragmented.convert(&pcm).unwrap(),
                complete.convert(&pcm).unwrap()
            );
        }
    }
}
