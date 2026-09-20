//! Cuts a recording into pieces the recognizer can take: at most 25 s each, cut at pauses
//! so a sentence is not split, with long silences left out so the model has nothing to
//! invent words for.

use crate::audio::rms;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct ChunkerConfig {
    pub max_chunk_seconds: f32,
    /// A gap at least this long can be cut at. Shorter dips are part of a word.
    pub min_pause_seconds: f32,
    /// A gap this long always ends the chunk, so long silences are never sent.
    pub long_pause_seconds: f32,
    /// Kept before and after speech so word onsets and tails are not clipped.
    pub padding_seconds: f32,
    /// Chunks with less speech than this are dropped as clicks and coughs.
    pub min_speech_seconds: f32,
}

impl Default for ChunkerConfig {
    fn default() -> Self {
        Self {
            max_chunk_seconds: 25.0,
            min_pause_seconds: 0.15,
            long_pause_seconds: 2.0,
            padding_seconds: 0.1,
            min_speech_seconds: 0.3,
        }
    }
}

const FRAME_SECONDS: f32 = 0.03;

pub fn speech_chunks(samples: &[f32], rate: u32, config: ChunkerConfig) -> Vec<Chunk> {
    let frame_len = ((rate as f32 * FRAME_SECONDS) as usize).max(1);
    let levels = samples.chunks(frame_len).map(rms).collect::<Vec<_>>();
    if levels.is_empty() {
        return Vec::new();
    }

    let threshold = speech_threshold(&levels);
    let frames = |seconds: f32| ((seconds / FRAME_SECONDS).round() as usize).max(1);
    let utterances = utterances(&levels, threshold, frames(config.min_pause_seconds));

    let max_frames = frames(config.max_chunk_seconds);
    let long_pause = frames(config.long_pause_seconds);
    let min_speech = frames(config.min_speech_seconds);
    let padding = (rate as f32 * config.padding_seconds) as usize;

    // Pack utterances into chunks: keep adding while the span fits, close at a long pause.
    let mut spans: Vec<(usize, usize, usize)> = Vec::new(); // start frame, end frame, speech frames
    for (start, end) in utterances {
        let speech = end - start;
        match spans.last_mut() {
            Some((span_start, span_end, span_speech))
                if start - *span_end < long_pause && end - *span_start <= max_frames =>
            {
                *span_end = end;
                *span_speech += speech;
            }
            _ => spans.push((start, end, speech)),
        }
    }

    spans
        .into_iter()
        .filter(|(_, _, speech)| *speech >= min_speech)
        // Speech that never pauses has to be cut somewhere.
        .flat_map(|(start, end, _)| {
            (start..end)
                .step_by(max_frames)
                .map(move |piece| (piece, (piece + max_frames).min(end)))
        })
        .map(|(start, end)| Chunk {
            start: (start * frame_len).saturating_sub(padding),
            end: (end * frame_len + padding).min(samples.len()),
        })
        .collect()
}

/// Anything clearly above the room's own noise counts as speech. The floor is taken from
/// the quietest tenth of the recording and clamped, so neither a silent room nor a
/// recording that is all speech moves it to a useless place.
fn speech_threshold(levels: &[f32]) -> f32 {
    let mut sorted = levels.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let floor = sorted[sorted.len() / 10];
    (floor * 3.0).clamp(0.003, 0.03)
}

/// Runs of speech frames as `(start, end)` frame ranges, bridging gaps shorter than
/// `min_pause` frames.
fn utterances(levels: &[f32], threshold: f32, min_pause: usize) -> Vec<(usize, usize)> {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for (index, level) in levels.iter().enumerate() {
        if *level <= threshold {
            continue;
        }
        match runs.last_mut() {
            Some((_, end)) if index - *end < min_pause => *end = index + 1,
            _ => runs.push((index, index + 1)),
        }
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;

    /// `(seconds, is_speech)` pieces; speech is a tone, the rest faint noise.
    fn recording(pieces: &[(f32, bool)]) -> Vec<f32> {
        let mut samples = Vec::new();
        for (seconds, is_speech) in pieces {
            for i in 0..(RATE as f32 * seconds) as usize {
                let t = i as f32 / RATE as f32;
                samples.push(if *is_speech {
                    0.3 * (2.0 * std::f32::consts::PI * 220.0 * t).sin()
                } else {
                    0.0005 * (2.0 * std::f32::consts::PI * 50.0 * t).sin()
                });
            }
        }
        samples
    }

    fn seconds(chunks: &[Chunk]) -> Vec<(f32, f32)> {
        chunks
            .iter()
            .map(|chunk| (chunk.start as f32 / RATE as f32, chunk.end as f32 / RATE as f32))
            .collect()
    }

    #[test]
    fn short_pauses_stay_inside_one_chunk() {
        let samples = recording(&[(1.0, false), (3.0, true), (0.4, false), (4.0, true), (1.0, false)]);

        let chunks = seconds(&speech_chunks(&samples, RATE, ChunkerConfig::default()));

        assert_eq!(chunks.len(), 1);
        assert!((chunks[0].0 - 0.9).abs() < 0.1, "{chunks:?}");
        assert!((chunks[0].1 - 8.5).abs() < 0.1, "{chunks:?}");
    }

    #[test]
    fn a_long_silence_is_left_out() {
        let samples = recording(&[(2.0, true), (5.0, false), (2.0, true)]);

        let chunks = seconds(&speech_chunks(&samples, RATE, ChunkerConfig::default()));

        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].1 < 2.3 && chunks[1].0 > 6.7, "{chunks:?}");
    }

    #[test]
    fn a_chunk_is_closed_at_a_pause_before_it_gets_too_long() {
        // Three 10 s utterances: the third does not fit in 25 s, so the cut lands in the
        // pause before it and not in the middle of speech.
        let samples = recording(&[
            (10.0, true),
            (0.5, false),
            (10.0, true),
            (0.5, false),
            (10.0, true),
        ]);

        let chunks = seconds(&speech_chunks(&samples, RATE, ChunkerConfig::default()));

        assert_eq!(chunks.len(), 2);
        assert!((chunks[0].1 - 20.6).abs() < 0.15, "{chunks:?}");
        assert!((chunks[1].0 - 20.9).abs() < 0.15, "{chunks:?}");
    }

    #[test]
    fn speech_that_never_pauses_is_still_cut_to_size() {
        let samples = recording(&[(60.0, true)]);

        let chunks = seconds(&speech_chunks(&samples, RATE, ChunkerConfig::default()));

        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|(start, end)| end - start <= 25.3), "{chunks:?}");
    }

    #[test]
    fn silence_and_clicks_give_nothing_to_transcribe() {
        let samples = recording(&[(3.0, false), (0.1, true), (3.0, false)]);

        assert!(speech_chunks(&samples, RATE, ChunkerConfig::default()).is_empty());
        assert!(speech_chunks(&[], RATE, ChunkerConfig::default()).is_empty());
    }
}
