use std::path::Path;

/// Everything downstream (chunking, the recognizer) works on 16 kHz mono.
pub const TARGET_RATE: u32 = 16_000;

const FILTER_TAPS: usize = 63;

/// Streaming mono resampler to [`TARGET_RATE`]: a windowed-sinc low-pass at the source
/// rate, then linear interpolation. Speech only needs the band below 8 kHz, and after the
/// low-pass the signal is oversampled enough for linear interpolation to be clean.
pub struct Resampler {
    step: f64,
    position: f64,
    previous: f32,
    taps: Vec<f32>,
    ring: Vec<f32>,
    ring_at: usize,
}

impl Resampler {
    pub fn new(source_rate: u32) -> Self {
        let source_rate = source_rate.max(1);
        // Nothing to remove when the source already sits at or below the target rate.
        let taps = if source_rate > TARGET_RATE {
            low_pass(0.45 * TARGET_RATE as f64 / source_rate as f64)
        } else {
            vec![1.0]
        };

        Self {
            step: source_rate as f64 / TARGET_RATE as f64,
            position: 0.0,
            previous: 0.0,
            ring: vec![0.0; taps.len()],
            ring_at: 0,
            taps,
        }
    }

    pub fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        let len = self.ring.len();
        for &sample in input {
            self.ring[self.ring_at] = sample;
            self.ring_at = (self.ring_at + 1) % len;

            let mut filtered = 0.0;
            let mut at = self.ring_at;
            for tap in &self.taps {
                filtered += tap * self.ring[at];
                at = (at + 1) % len;
            }

            // Output samples that fall between the previous input sample and this one.
            while self.position < 1.0 {
                output.push(self.previous + (filtered - self.previous) * self.position as f32);
                self.position += self.step;
            }
            self.position -= 1.0;
            self.previous = filtered;
        }
    }
}

/// Hamming-windowed sinc, `cutoff` as a fraction of the sampling rate, unity gain at DC.
fn low_pass(cutoff: f64) -> Vec<f32> {
    let middle = (FILTER_TAPS - 1) as f64 / 2.0;
    let mut taps = (0..FILTER_TAPS)
        .map(|i| {
            let x = i as f64 - middle;
            let sinc = if x == 0.0 {
                2.0 * cutoff
            } else {
                (2.0 * std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x)
            };
            let window = 0.54
                - 0.46 * (2.0 * std::f64::consts::PI * i as f64 / (FILTER_TAPS - 1) as f64).cos();
            sinc * window
        })
        .collect::<Vec<_>>();
    let gain = taps.iter().sum::<f64>();
    taps.iter_mut().for_each(|tap| *tap /= gain);
    taps.into_iter().map(|tap| tap as f32).collect()
}

/// Averages interleaved channels into one.
pub fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

/// Reads a WAV file as 16 kHz mono, whatever its own rate, width and channel count.
pub fn read_wav_16k_mono(path: &Path) -> Result<Vec<f32>, String> {
    let channels = read_wav_16k_channels(path)?;
    let count = channels.len().max(1) as f32;
    let mut channels = channels.into_iter();
    let mut mono = channels.next().unwrap_or_default();
    for channel in channels {
        for (sum, sample) in mono.iter_mut().zip(channel) {
            *sum += sample;
        }
    }
    mono.iter_mut().for_each(|sample| *sample /= count);
    Ok(mono)
}

/// Reads a WAV file as 16 kHz, one vector per channel.
pub fn read_wav_16k_channels(path: &Path) -> Result<Vec<Vec<f32>>, String> {
    let mut reader = hound::WavReader::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let spec = reader.spec();

    let interleaved = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?,
        hound::SampleFormat::Int => {
            let scale = (1_i64 << (spec.bits_per_sample.max(1) - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|sample| sample.map(|value| value as f32 / scale))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?
        }
    };

    let count = spec.channels.max(1) as usize;
    Ok((0..count)
        .map(|channel| {
            let samples = interleaved.iter().skip(channel).step_by(count).copied().collect::<Vec<_>>();
            if spec.sample_rate == TARGET_RATE {
                return samples;
            }
            let mut resampled = Vec::with_capacity(samples.len() / 2);
            Resampler::new(spec.sample_rate).process(&samples, &mut resampled);
            resampled
        })
        .collect())
}

/// Signals the tests of several modules share.
#[cfg(test)]
pub(crate) mod test_signals {
    use super::TARGET_RATE;

    const RATE: usize = TARGET_RATE as usize;

    /// Small deterministic noise source, so the tests need no dependency and never flake.
    pub struct Noise(pub u64);

    impl Noise {
        pub fn next(&mut self) -> f32 {
            self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 33) as f32 / (1u64 << 31) as f32) * 2.0 - 1.0
        }
    }

    /// Something shaped like talking: syllables of noise that swell and fade, with short
    /// gaps, only inside the given `(from, to)` stretches of seconds.
    pub fn talk(seconds: usize, turns: &[(f32, f32)], level: f32, seed: u64) -> Vec<f32> {
        let mut noise = Noise(seed);
        let mut samples = vec![0.0; seconds * RATE];
        for (from, to) in turns {
            let mut at = (from * RATE as f32) as usize;
            let end = ((to * RATE as f32) as usize).min(samples.len());
            while at < end {
                let syllable = (0.12 + 0.2 * noise.next().abs()) * RATE as f32;
                let loudness = level * (0.4 + 0.6 * noise.next().abs());
                let length = (syllable as usize).min(end - at);
                for i in 0..length {
                    let shape = (std::f32::consts::PI * i as f32 / length as f32).sin();
                    samples[at + i] = loudness * shape * noise.next();
                }
                at += length + (0.05 * RATE as f32) as usize;
            }
        }
        samples
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(frequency: f32, rate: u32, seconds: f32) -> Vec<f32> {
        (0..(rate as f32 * seconds) as usize)
            .map(|i| (2.0 * std::f32::consts::PI * frequency * i as f32 / rate as f32).sin())
            .collect()
    }

    fn resample(input: &[f32], rate: u32) -> Vec<f32> {
        let mut output = Vec::new();
        let mut resampler = Resampler::new(rate);
        // Uneven block sizes, as an audio callback delivers them.
        for block in input.chunks(441) {
            resampler.process(block, &mut output);
        }
        output
    }

    #[test]
    fn speech_band_survives_with_the_right_length() {
        for rate in [48_000, 44_100] {
            let output = resample(&sine(1_000.0, rate, 1.0), rate);

            assert!((output.len() as i64 - 16_000).abs() <= 2, "{rate}: {}", output.len());
            let level = rms(&output[1_000..]);
            assert!((level - 0.707).abs() < 0.04, "{rate}: rms {level}");
        }
    }

    #[test]
    fn content_above_the_new_nyquist_is_removed_instead_of_aliasing() {
        let output = resample(&sine(12_000.0, 48_000, 1.0), 48_000);

        assert!(rms(&output[1_000..]) < 0.02);
    }

    #[test]
    fn a_source_already_at_16k_passes_through() {
        let input = sine(440.0, 16_000, 0.5);

        assert_eq!(resample(&input, 16_000).len(), input.len());
    }

    #[test]
    fn stereo_is_averaged() {
        assert_eq!(downmix(&[1.0, 0.0, 0.5, 0.5], 2), vec![0.5, 0.5]);
    }

    #[test]
    fn wav_files_are_read_as_16k_mono() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stereo-48k.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for sample in sine(500.0, 48_000, 1.0) {
            let value = (sample * 16_000.0) as i16;
            writer.write_sample(value).unwrap();
            writer.write_sample(value).unwrap();
        }
        writer.finalize().unwrap();

        let samples = read_wav_16k_mono(&path).unwrap();

        assert!((samples.len() as i64 - 16_000).abs() <= 2);
        assert!((rms(&samples[1_000..]) - 0.345).abs() < 0.03);
    }

    #[test]
    fn channels_are_kept_apart_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("call.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: TARGET_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for _ in 0..1_000 {
            writer.write_sample(8_192i16).unwrap();
            writer.write_sample(-16_384i16).unwrap();
        }
        writer.finalize().unwrap();

        let channels = read_wav_16k_channels(&path).unwrap();

        assert_eq!(channels.len(), 2);
        assert!(channels[0].iter().all(|sample| *sample == 0.25));
        assert!(channels[1].iter().all(|sample| *sample == -0.5));
    }
}
