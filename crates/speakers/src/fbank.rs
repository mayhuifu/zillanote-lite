//! Kaldi-style log mel filterbank features, the input of the speaker embedding model.
//!
//! The model was trained on features from Kaldi's `compute-fbank-feats` with 80 bins, a
//! 25 ms povey window every 10 ms, 0.97 pre-emphasis, no dither, the waveform in 16-bit
//! range, and the mean over time removed. This follows that recipe step by step, so the
//! embeddings match what the C++ library gives without building it.

pub const NUM_BINS: usize = 80;

const SAMPLE_RATE: f32 = 16_000.0;
const FRAME_LENGTH: usize = 400;
const FRAME_SHIFT: usize = 160;
const FFT_SIZE: usize = 512;
const PRE_EMPHASIS: f32 = 0.97;
const LOW_FREQUENCY: f32 = 20.0;

pub struct Fbank {
    window: Vec<f32>,
    /// Per mel bin: the first FFT bin it covers and its weights from there on.
    filters: Vec<(usize, Vec<f32>)>,
    /// `e^(-2πik/N)` for the first half of the circle.
    twiddles: Vec<(f32, f32)>,
}

impl Default for Fbank {
    fn default() -> Self {
        Self::new()
    }
}

impl Fbank {
    pub fn new() -> Self {
        let window = (0..FRAME_LENGTH)
            .map(|i| {
                let phase = 2.0 * std::f64::consts::PI * i as f64 / (FRAME_LENGTH - 1) as f64;
                (0.5 - 0.5 * phase.cos()).powf(0.85) as f32
            })
            .collect();
        let twiddles = (0..FFT_SIZE / 2)
            .map(|k| {
                let angle = -2.0 * std::f64::consts::PI * k as f64 / FFT_SIZE as f64;
                (angle.cos() as f32, angle.sin() as f32)
            })
            .collect();
        Self {
            window,
            filters: mel_filters(),
            twiddles,
        }
    }

    pub fn frame_count(samples: usize) -> usize {
        match samples.checked_sub(FRAME_LENGTH) {
            Some(rest) => 1 + rest / FRAME_SHIFT,
            None => 0,
        }
    }

    /// `frame_count × NUM_BINS` values, row by row. `samples` are in `[-1, 1]`.
    pub fn compute(&self, samples: &[f32]) -> Vec<f32> {
        let frames = Self::frame_count(samples.len());
        let mut features = Vec::with_capacity(frames * NUM_BINS);
        let mut real = vec![0.0f32; FFT_SIZE];
        let mut imaginary = vec![0.0f32; FFT_SIZE];

        for frame in 0..frames {
            let source = &samples[frame * FRAME_SHIFT..frame * FRAME_SHIFT + FRAME_LENGTH];
            for (value, sample) in real.iter_mut().zip(source) {
                *value = sample * 32_768.0;
            }
            let mean = real[..FRAME_LENGTH].iter().sum::<f32>() / FRAME_LENGTH as f32;
            real[..FRAME_LENGTH].iter_mut().for_each(|value| *value -= mean);
            for i in (1..FRAME_LENGTH).rev() {
                real[i] -= PRE_EMPHASIS * real[i - 1];
            }
            real[0] -= PRE_EMPHASIS * real[0];
            for (value, weight) in real.iter_mut().zip(&self.window) {
                *value *= weight;
            }
            real[FRAME_LENGTH..].fill(0.0);
            imaginary.fill(0.0);
            self.fft(&mut real, &mut imaginary);

            for (first, weights) in &self.filters {
                let energy = weights
                    .iter()
                    .enumerate()
                    .map(|(offset, weight)| {
                        let bin = first + offset;
                        weight * (real[bin] * real[bin] + imaginary[bin] * imaginary[bin])
                    })
                    .sum::<f32>();
                features.push(energy.max(f32::EPSILON).ln());
            }
        }

        // The mean over time goes, bin by bin: it is the channel, not the voice.
        if frames > 0 {
            for bin in 0..NUM_BINS {
                let mean = (0..frames).map(|frame| features[frame * NUM_BINS + bin]).sum::<f32>() / frames as f32;
                (0..frames).for_each(|frame| features[frame * NUM_BINS + bin] -= mean);
            }
        }
        features
    }

    /// In-place radix-2 transform of `FFT_SIZE` points.
    fn fft(&self, real: &mut [f32], imaginary: &mut [f32]) {
        let n = FFT_SIZE;
        let mut j = 0;
        for i in 1..n {
            let mut bit = n >> 1;
            while j & bit != 0 {
                j ^= bit;
                bit >>= 1;
            }
            j |= bit;
            if i < j {
                real.swap(i, j);
                imaginary.swap(i, j);
            }
        }

        let mut size = 2;
        while size <= n {
            let half = size / 2;
            let stride = n / size;
            for start in (0..n).step_by(size) {
                for k in 0..half {
                    let (cos, sin) = self.twiddles[k * stride];
                    let (a, b) = (start + k, start + k + half);
                    let (re, im) = (real[b] * cos - imaginary[b] * sin, real[b] * sin + imaginary[b] * cos);
                    real[b] = real[a] - re;
                    imaginary[b] = imaginary[a] - im;
                    real[a] += re;
                    imaginary[a] += im;
                }
            }
            size *= 2;
        }
    }
}

fn mel(frequency: f32) -> f32 {
    1127.0 * (1.0 + frequency / 700.0).ln()
}

/// Triangles evenly spaced on the mel scale from 20 Hz to half the sampling rate.
fn mel_filters() -> Vec<(usize, Vec<f32>)> {
    let bins = FFT_SIZE / 2;
    let bin_width = SAMPLE_RATE / FFT_SIZE as f32;
    let low = mel(LOW_FREQUENCY);
    let step = (mel(SAMPLE_RATE / 2.0) - low) / (NUM_BINS + 1) as f32;

    (0..NUM_BINS)
        .map(|index| {
            let left = low + index as f32 * step;
            let center = left + step;
            let right = center + step;
            let mut first = None;
            let mut weights = Vec::new();
            for bin in 0..bins {
                let at = mel(bin_width * bin as f32);
                if at > left && at < right {
                    first.get_or_insert(bin);
                    weights.push(if at <= center {
                        (at - left) / (center - left)
                    } else {
                        (right - at) / (right - center)
                    });
                }
            }
            (first.unwrap_or(0), weights)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frequency: f32, seconds: f32) -> Vec<f32> {
        (0..(SAMPLE_RATE * seconds) as usize)
            .map(|i| 0.5 * (2.0 * std::f32::consts::PI * frequency * i as f32 / SAMPLE_RATE).sin())
            .collect()
    }

    /// Values kaldi-native-fbank gives for the same signal, with the options of the module
    /// comment. The speaker model was trained on its features, so these must not drift.
    #[test]
    fn features_match_kaldi_native_fbank() {
        const REFERENCE: [(usize, usize, f32); 7] = [
            (0, 0, 2.23499),
            (0, 5, 3.56647),
            (10, 40, -4.08566),
            (30, 79, 0.17603),
            (60, 5, -3.18821),
            (60, 33, 0.07814),
            (97, 60, 0.15474),
        ];
        let mut seed: u32 = 0x1234_5678;
        let samples = (0..16_000)
            .map(|i| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let noise = (seed as f32 / u32::MAX as f32) * 2.0 - 1.0;
                let seconds = i as f32 / SAMPLE_RATE;
                let tone = if i < 8_000 { 220.0 } else { 1_800.0 };
                0.3 * (2.0 * std::f32::consts::PI * tone * seconds).sin() + 0.05 * noise
            })
            .collect::<Vec<_>>();

        let features = Fbank::new().compute(&samples);

        for (frame, bin, expected) in REFERENCE {
            let value = features[frame * NUM_BINS + bin];
            assert!((value - expected).abs() < 2e-3, "frame {frame}, bin {bin}: {value} against {expected}");
        }
    }

    #[test]
    fn frames_follow_kaldis_count() {
        assert_eq!(Fbank::frame_count(399), 0);
        assert_eq!(Fbank::frame_count(400), 1);
        assert_eq!(Fbank::frame_count(16_000), 98);
        assert_eq!(Fbank::new().compute(&tone(440.0, 1.0)).len(), 98 * NUM_BINS);
    }

    #[test]
    fn the_transform_finds_a_tone_in_its_bin() {
        let fbank = Fbank::new();
        let mut real = (0..FFT_SIZE)
            .map(|i| (2.0 * std::f32::consts::PI * 32.0 * i as f32 / FFT_SIZE as f32).cos())
            .collect::<Vec<_>>();
        let mut imaginary = vec![0.0; FFT_SIZE];

        fbank.fft(&mut real, &mut imaginary);

        for bin in 0..FFT_SIZE {
            let magnitude = (real[bin] * real[bin] + imaginary[bin] * imaginary[bin]).sqrt();
            let expected = if bin == 32 || bin == FFT_SIZE - 32 { FFT_SIZE as f32 / 2.0 } else { 0.0 };
            assert!((magnitude - expected).abs() < 0.01, "bin {bin}: {magnitude}");
        }
    }

    #[test]
    fn a_higher_tone_lights_up_a_higher_bin() {
        let fbank = Fbank::new();
        // Two tones in turn, so removing the mean over time leaves each one standing out.
        let mut samples = tone(300.0, 1.0);
        samples.extend(tone(3_000.0, 1.0));
        let features = fbank.compute(&samples);
        let loudest = |frame: usize| {
            let row = &features[frame * NUM_BINS..(frame + 1) * NUM_BINS];
            (0..NUM_BINS).max_by(|a, b| row[*a].total_cmp(&row[*b])).unwrap()
        };

        assert!(loudest(40) < loudest(150), "{} then {}", loudest(40), loudest(150));
    }

    #[test]
    fn every_bin_averages_to_zero_over_time() {
        let features = Fbank::new().compute(&tone(700.0, 2.0));
        let frames = features.len() / NUM_BINS;

        for bin in 0..NUM_BINS {
            let mean = (0..frames).map(|frame| features[frame * NUM_BINS + bin]).sum::<f32>() / frames as f32;
            assert!(mean.abs() < 1e-3, "bin {bin}: {mean}");
        }
    }

    #[test]
    fn the_filters_cover_the_band_without_gaps() {
        let filters = mel_filters();

        assert_eq!(filters.len(), NUM_BINS);
        assert!(filters.iter().all(|(_, weights)| !weights.is_empty()));
        assert!(filters.windows(2).all(|pair| pair[0].0 <= pair[1].0));
        let (first, weights) = filters.last().unwrap();
        assert_eq!(first + weights.len(), FFT_SIZE / 2);
    }
}
