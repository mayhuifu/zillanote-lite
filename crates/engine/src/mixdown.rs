//! From the two recorded channels to the one signal the recognizer hears.
//!
//! With headphones the microphone holds this side of a call and the computer's channel the
//! other side, and the sum is the whole conversation. On loudspeakers the microphone hears
//! the other side as well, a little later and with the room added, and the sum would say
//! everything twice. So where the microphone holds nothing but that echo it is left out: the
//! computer's channel already has those words, clean.
//!
//! The echo is recognized by how the two loudness curves move together, never by loudness
//! alone: people talking in a room while the computer plays something are not an echo, and
//! must not be silenced.

use crate::audio::{TARGET_RATE, rms};

const FRAME: usize = (TARGET_RATE as usize * 30) / 1000;
/// The computer is playing something.
const ACTIVE: f32 = 0.003;
/// Loud enough for the ratio between the two channels to mean something.
const CLEAR: f32 = 0.01;
/// Less evidence than this (three seconds) and nothing is concluded.
const MIN_ACTIVE_FRAMES: usize = 100;
/// How far the echo may trail the computer's channel (360 ms), and how far the recorder's
/// own timing may put it ahead.
const MAX_LAG: isize = 12;
const MIN_LAG: isize = -2;
/// The loudness curves have to agree at least this well for there to be an echo at all.
const MIN_CORRELATION: f32 = 0.5;
/// The microphone counts as echo while it is no louder than twice what the echo explains.
const HEADROOM: f32 = 2.0;
/// Frames of room decay still counted as echo after the computer goes quiet.
const TAIL: isize = 3;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Echo {
    /// How many frames the microphone trails the computer's channel.
    pub lag: isize,
    /// Echo level in the microphone relative to the level in the computer's channel.
    pub gain: f32,
}

/// The signal to transcribe. One channel passes through; two are (microphone, computer).
pub fn recognition_signal(mut channels: Vec<Vec<f32>>) -> Vec<f32> {
    match channels.len() {
        0 => Vec::new(),
        1 => channels.remove(0),
        _ => mix_call(&channels[0], &channels[1]),
    }
}

pub fn mix_call(microphone: &[f32], system: &[f32]) -> Vec<f32> {
    let microphone_levels = levels(microphone);
    let system_levels = levels(system);
    let keep = match find_echo(&microphone_levels, &system_levels) {
        Some(echo) => {
            tracing::info!(lag_ms = echo.lag * 30, gain = echo.gain, "echo_found");
            frames_to_keep(&microphone_levels, &system_levels, echo)
        }
        None => vec![true; microphone_levels.len()],
    };

    let mut mixed = Vec::with_capacity(microphone.len().max(system.len()));
    let mut previous = 1.0f32;
    for (index, frame) in microphone.chunks(FRAME).enumerate() {
        // The gain glides over the frame, so taking the echo out never clicks.
        let target = if keep[index] { 1.0 } else { 0.0 };
        for (at, sample) in frame.iter().enumerate() {
            let gain = previous + (target - previous) * (at + 1) as f32 / FRAME as f32;
            mixed.push(sample * gain);
        }
        previous = target;
    }
    mixed.resize(microphone.len().max(system.len()), 0.0);
    for (sum, sample) in mixed.iter_mut().zip(system) {
        *sum = (*sum + sample).clamp(-1.0, 1.0);
    }
    mixed
}

fn levels(samples: &[f32]) -> Vec<f32> {
    samples.chunks(FRAME).map(rms).collect()
}

/// Whether the microphone carries an echo of the computer's channel, and how late and how
/// loud. Judged only where the computer plays, by the lag at which the two loudness curves
/// agree best.
pub fn find_echo(microphone: &[f32], system: &[f32]) -> Option<Echo> {
    let active = (0..system.len()).filter(|at| system[*at] > ACTIVE).collect::<Vec<_>>();
    if active.len() < MIN_ACTIVE_FRAMES {
        return None;
    }

    let at_lag = |lag: isize| {
        active
            .iter()
            .filter_map(move |at| {
                let shifted = usize::try_from(*at as isize + lag).ok()?;
                Some((system[*at], *microphone.get(shifted)?))
            })
            .collect::<Vec<_>>()
    };
    let (lag, correlation) = (MIN_LAG..=MAX_LAG)
        .map(|lag| (lag, correlation(&at_lag(lag))))
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    if correlation < MIN_CORRELATION {
        return None;
    }

    // The middle ratio: this side talking over the echo pushes some ratios up, not the middle.
    let mut ratios = at_lag(lag)
        .into_iter()
        .filter(|(system, _)| *system > CLEAR)
        .map(|(system, microphone)| microphone / system)
        .collect::<Vec<_>>();
    if ratios.is_empty() {
        return None;
    }
    ratios.sort_by(|a, b| a.total_cmp(b));
    Some(Echo {
        lag,
        gain: ratios[ratios.len() / 2],
    })
}

fn correlation(pairs: &[(f32, f32)]) -> f32 {
    if pairs.len() < MIN_ACTIVE_FRAMES {
        return 0.0;
    }
    let count = pairs.len() as f32;
    let mean_a = pairs.iter().map(|(a, _)| a).sum::<f32>() / count;
    let mean_b = pairs.iter().map(|(_, b)| b).sum::<f32>() / count;
    let (mut both, mut only_a, mut only_b) = (0.0f32, 0.0f32, 0.0f32);
    for (a, b) in pairs {
        both += (a - mean_a) * (b - mean_b);
        only_a += (a - mean_a) * (a - mean_a);
        only_b += (b - mean_b) * (b - mean_b);
    }
    if only_a <= f32::EPSILON || only_b <= f32::EPSILON {
        return 0.0;
    }
    both / (only_a * only_b).sqrt()
}

/// A microphone frame goes when the echo expected there explains how loud it is. Someone
/// on this side talking over the far side is louder than that, and stays.
fn frames_to_keep(microphone: &[f32], system: &[f32], echo: Echo) -> Vec<bool> {
    (0..microphone.len() as isize)
        .map(|at| {
            let source = at - echo.lag;
            let loudest = (source - TAIL..=source + 1)
                .filter_map(|at| system.get(usize::try_from(at).ok()?))
                .fold(0.0f32, |loudest, level| loudest.max(*level));
            loudest <= ACTIVE || microphone[at as usize] > HEADROOM * echo.gain * loudest
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::test_signals::{Noise, talk};

    const RATE: usize = TARGET_RATE as usize;

    /// What a microphone hears of the loudspeakers: later, quieter, with a little room.
    fn echo_of(system: &[f32], delay_ms: usize, gain: f32) -> Vec<f32> {
        let delay = delay_ms * RATE / 1000;
        let room = 40 * RATE / 1000;
        (0..system.len())
            .map(|at| {
                let direct = at.checked_sub(delay).map_or(0.0, |at| system[at]);
                let reflected = at.checked_sub(delay + room).map_or(0.0, |at| system[at]);
                gain * (direct + 0.4 * reflected)
            })
            .collect()
    }

    fn add(a: &[f32], b: &[f32]) -> Vec<f32> {
        a.iter().zip(b).map(|(a, b)| a + b).collect()
    }

    fn level(samples: &[f32], from: f32, to: f32) -> f32 {
        rms(&samples[(from * RATE as f32) as usize..(to * RATE as f32) as usize])
    }

    const FAR_SIDE: &[(f32, f32)] = &[(1.0, 9.0), (14.0, 22.0)];
    const THIS_SIDE: &[(f32, f32)] = &[(10.0, 13.0), (23.0, 28.0)];

    #[test]
    fn with_headphones_both_sides_are_simply_added() {
        let system = talk(30, FAR_SIDE, 0.2, 1);
        let room_noise = talk(30, &[(0.0, 30.0)], 0.002, 2);
        let microphone = add(&talk(30, THIS_SIDE, 0.2, 3), &room_noise);

        assert_eq!(find_echo(&levels(&microphone), &levels(&system)), None);
        assert_eq!(mix_call(&microphone, &system), add(&microphone, &system));
    }

    #[test]
    fn on_loudspeakers_the_far_side_is_heard_once_and_this_side_is_kept() {
        let system = talk(30, FAR_SIDE, 0.2, 1);
        let this_side = talk(30, THIS_SIDE, 0.2, 3);
        let microphone = add(&this_side, &echo_of(&system, 70, 0.6));

        let echo = find_echo(&levels(&microphone), &levels(&system)).expect("an echo");
        assert!((2..=4).contains(&echo.lag), "{echo:?}");
        assert!((0.4..0.9).contains(&echo.gain), "{echo:?}");

        let mixed = mix_call(&microphone, &system);
        let doubled = add(&microphone, &system);
        // While the far side talks, the mix is their channel and next to nothing else.
        let residue = mixed.iter().zip(&system).map(|(mixed, system)| mixed - system).collect::<Vec<_>>();
        let echo_before = level(&doubled.iter().zip(&system).map(|(d, s)| d - s).collect::<Vec<_>>(), 1.0, 9.0);
        assert!(level(&residue, 1.0, 9.0) < 0.1 * echo_before, "{} of {echo_before}", level(&residue, 1.0, 9.0));
        // While this side talks, the mix is the microphone, whole.
        assert!((level(&mixed, 10.0, 13.0) - level(&this_side, 10.0, 13.0)).abs() < 0.002);
        assert!((level(&mixed, 23.0, 28.0) - level(&this_side, 23.0, 28.0)).abs() < 0.002);
    }

    #[test]
    fn talking_over_the_far_side_on_loudspeakers_stays_in() {
        let system = talk(30, &[(1.0, 20.0)], 0.1, 1);
        // Close to the microphone, so well above the echo of the loudspeakers.
        let interruption = talk(30, &[(8.0, 11.0)], 0.4, 3);
        let microphone = add(&interruption, &echo_of(&system, 70, 0.5));

        let mixed = mix_call(&microphone, &system);

        let kept = level(&mixed, 8.0, 11.0);
        assert!(kept > 0.8 * level(&add(&microphone, &system), 8.0, 11.0), "{kept}");
    }

    #[test]
    fn people_in_a_room_are_never_silenced_by_something_the_computer_plays() {
        // A steady sound from the computer, picked up by the microphone too, under a
        // conversation in the room.
        let mut noise = Noise(7);
        let system = (0..30 * RATE).map(|_| 0.03 * noise.next()).collect::<Vec<_>>();
        let room = talk(30, &[(1.0, 28.0)], 0.2, 3);
        let microphone = add(&room, &echo_of(&system, 70, 0.5));

        let mixed = mix_call(&microphone, &system);

        assert_eq!(mixed, add(&microphone, &system));
    }

    #[test]
    fn a_computer_that_stayed_quiet_changes_nothing() {
        let microphone = talk(10, &[(1.0, 9.0)], 0.2, 3);

        assert_eq!(mix_call(&microphone, &vec![0.0; microphone.len()]), microphone);
        assert_eq!(recognition_signal(vec![microphone.clone()]), microphone);
        assert!(recognition_signal(Vec::new()).is_empty());
    }
}
