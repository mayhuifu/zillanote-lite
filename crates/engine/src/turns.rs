//! Cuts the recognizer's chunks where the speaker changes, so that every line of the
//! transcript belongs to one speaker.
//!
//! The recognizer gives one text per chunk and no word times, so a chunk with two voices in
//! it could only be given to one of them. Cutting first keeps both. A change has to last a
//! second to count: a "yeah" thrown in stays on the line of whoever is talking, because a
//! piece that short is too little for the recognizer to work with.

use crate::audio::rms;
use crate::chunker::Chunk;

/// Someone talking from `start` to `end` seconds. Turns of different speakers may overlap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Turn {
    pub start: f64,
    pub end: f64,
    pub speaker: usize,
}

/// A range of samples to transcribe, and who talks in it, when known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    pub start: usize,
    pub end: usize,
    pub speaker: Option<usize>,
}

const STEP_SECONDS: f64 = 0.02;
/// Shorter runs are given to a neighbour.
const MIN_PIECE_SECONDS: f64 = 1.0;
/// A cut moves to the quietest spot this close to where the speaker changes.
const SNAP_SECONDS: f64 = 0.25;
const SNAP_WINDOW_SECONDS: f64 = 0.03;

pub fn split_at_turns(chunks: &[Chunk], turns: &[Turn], samples: &[f32], rate: u32) -> Vec<Piece> {
    let step = (rate as f64 * STEP_SECONDS) as usize;
    let mut timeline: Vec<Option<usize>> = vec![None; samples.len() / step.max(1) + 1];
    // Whoever was already talking keeps the moments two people share.
    let mut ordered = turns.to_vec();
    ordered.sort_by(|a, b| a.start.total_cmp(&b.start));
    for turn in ordered {
        let from = (turn.start / STEP_SECONDS) as usize;
        let to = ((turn.end / STEP_SECONDS).ceil() as usize).min(timeline.len());
        for slot in timeline.iter_mut().take(to).skip(from) {
            slot.get_or_insert(turn.speaker);
        }
    }

    chunks
        .iter()
        .flat_map(|chunk| split_chunk(*chunk, &timeline, step, samples, rate))
        .collect()
}

fn split_chunk(chunk: Chunk, timeline: &[Option<usize>], step: usize, samples: &[f32], rate: u32) -> Vec<Piece> {
    // Runs of one speaker as (speaker, first step, step after the last). Steps nobody is
    // known to talk in go to the run before them.
    let mut runs: Vec<(usize, usize, usize)> = Vec::new();
    let (first, last) = (chunk.start / step, chunk.end.div_ceil(step).min(timeline.len()));
    for at in first..last {
        match (timeline[at], runs.last_mut()) {
            (Some(speaker), Some(run)) if run.0 == speaker => run.2 = at + 1,
            (Some(speaker), _) => runs.push((speaker, at, at + 1)),
            (None, Some(run)) => run.2 = at + 1,
            (None, None) => {}
        }
    }
    let Some(first_run) = runs.first_mut() else {
        return vec![Piece {
            start: chunk.start,
            end: chunk.end,
            speaker: None,
        }];
    };
    first_run.1 = first;

    // Runs too short to stand alone join a neighbour, shortest first.
    let min_steps = (MIN_PIECE_SECONDS / STEP_SECONDS) as usize;
    while runs.len() > 1 {
        let (at, shortest) = runs
            .iter()
            .enumerate()
            .map(|(at, run)| (at, run.2 - run.1))
            .min_by_key(|(_, length)| *length)
            .expect("more than one run");
        if shortest >= min_steps {
            break;
        }
        let (_, from, to) = runs.remove(at);
        match at.checked_sub(1) {
            Some(before) => runs[before].2 = to,
            None => runs[0].1 = from,
        }
        runs.dedup_by(|next, run| {
            let same = next.0 == run.0;
            if same {
                run.2 = next.2;
            }
            same
        });
    }

    let mut pieces = Vec::with_capacity(runs.len());
    let mut start = chunk.start;
    for (index, (speaker, _, to)) in runs.iter().enumerate() {
        let end = match runs.get(index + 1) {
            Some(_) => quietest_near(samples, to * step, start, chunk.end, rate),
            None => chunk.end,
        };
        pieces.push(Piece {
            start,
            end,
            speaker: Some(*speaker),
        });
        start = end;
    }
    pieces
}

/// The middle of the quietest short window around `at`, staying inside `(from, to)`.
fn quietest_near(samples: &[f32], at: usize, from: usize, to: usize, rate: u32) -> usize {
    let reach = (rate as f64 * SNAP_SECONDS) as usize;
    let window = (rate as f64 * SNAP_WINDOW_SECONDS) as usize;
    let low = at.saturating_sub(reach).max(from + window);
    let high = (at + reach).min(to.saturating_sub(window)).min(samples.len().saturating_sub(window));
    if low >= high {
        return at.clamp(from, to);
    }

    (low..high)
        .step_by(window / 2)
        .min_by(|a, b| rms(&samples[*a..*a + window]).total_cmp(&rms(&samples[*b..*b + window])))
        .map_or(at, |quietest| quietest + window / 2)
}

/// How long each speaker talks in all, in seconds, by speaker index.
pub fn seconds_by_speaker(turns: &[Turn]) -> Vec<f64> {
    let count = turns.iter().map(|turn| turn.speaker + 1).max().unwrap_or(0);
    let mut seconds = vec![0.0; count];
    for turn in turns {
        seconds[turn.speaker] += (turn.end - turn.start).max(0.0);
    }
    seconds
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;

    fn turn(start: f64, end: f64, speaker: usize) -> Turn {
        Turn { start, end, speaker }
    }

    fn chunk(start: f64, end: f64) -> Chunk {
        Chunk {
            start: (start * RATE as f64) as usize,
            end: (end * RATE as f64) as usize,
        }
    }

    fn seconds(pieces: &[Piece]) -> Vec<(f64, f64, Option<usize>)> {
        pieces
            .iter()
            .map(|piece| (piece.start as f64 / RATE as f64, piece.end as f64 / RATE as f64, piece.speaker))
            .collect()
    }

    /// Steady sound, silent for 100 ms around each of `pauses` (in seconds).
    fn sound(length: f64, pauses: &[f64]) -> Vec<f32> {
        (0..(length * RATE as f64) as usize)
            .map(|i| {
                let t = i as f64 / RATE as f64;
                let paused = pauses.iter().any(|pause| (t - pause).abs() < 0.05);
                if paused { 0.0 } else { 0.3 * (t * 1_500.0).sin() as f32 }
            })
            .collect()
    }

    #[test]
    fn a_chunk_with_one_speaker_stays_whole() {
        let pieces = split_at_turns(&[chunk(1.0, 9.0)], &[turn(0.5, 9.5, 0)], &sound(10.0, &[]), RATE);

        assert_eq!(seconds(&pieces), [(1.0, 9.0, Some(0))]);
    }

    #[test]
    fn a_change_of_speaker_cuts_the_chunk_at_the_pause_next_to_it() {
        // The speakers change at 5.0 s by the diarization; the pause is at 5.1 s.
        let turns = [turn(1.0, 5.0, 0), turn(5.0, 9.0, 1)];

        let pieces = seconds(&split_at_turns(&[chunk(1.0, 9.0)], &turns, &sound(10.0, &[5.1]), RATE));

        assert_eq!(pieces.len(), 2);
        assert_eq!((pieces[0].2, pieces[1].2), (Some(0), Some(1)));
        assert!((pieces[0].1 - 5.1).abs() < 0.04, "{pieces:?}");
        assert_eq!(pieces[0].1, pieces[1].0);
    }

    #[test]
    fn a_word_thrown_in_stays_on_the_line_of_whoever_is_talking() {
        let turns = [turn(1.0, 4.0, 0), turn(4.0, 4.4, 1), turn(4.4, 9.0, 0)];

        let pieces = split_at_turns(&[chunk(1.0, 9.0)], &turns, &sound(10.0, &[]), RATE);

        assert_eq!(seconds(&pieces), [(1.0, 9.0, Some(0))]);
    }

    #[test]
    fn whoever_was_talking_keeps_the_moments_two_people_share() {
        let turns = [turn(1.0, 6.0, 0), turn(5.0, 9.0, 1)];

        let pieces = seconds(&split_at_turns(&[chunk(1.0, 9.0)], &turns, &sound(10.0, &[]), RATE));

        assert_eq!(pieces.len(), 2);
        assert!((pieces[0].1 - 6.0).abs() < 0.3, "{pieces:?}");
    }

    #[test]
    fn speech_nobody_was_heard_in_has_no_speaker() {
        let pieces = split_at_turns(&[chunk(1.0, 3.0)], &[turn(20.0, 25.0, 0)], &sound(30.0, &[]), RATE);

        assert_eq!(seconds(&pieces), [(1.0, 3.0, None)]);
    }

    #[test]
    fn talking_time_is_added_up_per_speaker() {
        let turns = [turn(0.0, 4.0, 0), turn(4.0, 5.5, 1), turn(6.0, 8.0, 0)];

        assert_eq!(seconds_by_speaker(&turns), [6.0, 1.5]);
        assert!(seconds_by_speaker(&[]).is_empty());
    }
}
