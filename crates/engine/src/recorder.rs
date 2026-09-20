//! Records the microphone to a 16 kHz WAV file, and the computer's own sound next to it when
//! asked: one channel for the microphone alone, two (microphone, computer) with both.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::audio::{Resampler, TARGET_RATE, downmix, rms};
use crate::system_audio::SystemAudio;

const LEVEL_INTERVAL: Duration = Duration::from_millis(100);
/// How often the WAV header is brought up to date, so a crash loses seconds, not the file.
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct RecordingSummary {
    pub path: PathBuf,
    pub duration_seconds: f64,
    /// Whether the file has the computer's sound as a second channel.
    pub system_audio: bool,
}

pub struct Recording {
    stop: Arc<AtomicBool>,
    worker: JoinHandle<Result<RecordingSummary, String>>,
    started: Instant,
    system_audio_problem: Option<String>,
}

impl Recording {
    /// Starts recording and returns once audio is flowing. `on_level` gets the input
    /// level (RMS, 0 to 1) about ten times a second. With `system_audio`, the computer's
    /// own sound is recorded too; if that cannot start, the microphone is recorded alone
    /// and [`Recording::system_audio_problem`] says why.
    pub fn start(
        path: PathBuf,
        system_audio: bool,
        on_level: impl Fn(f32) + Send + 'static,
    ) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel();

        // Neither stream is `Send` on macOS, so they live and die on this thread.
        let worker = std::thread::Builder::new()
            .name("recorder".to_string())
            .spawn({
                let stop = stop.clone();
                move || record(&path, system_audio, &stop, ready_tx, on_level)
            })
            .map_err(|e| e.to_string())?;

        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(system_audio_problem)) => Ok(Self {
                stop,
                worker,
                started: Instant::now(),
                system_audio_problem,
            }),
            Ok(Err(error)) => Err(error),
            Err(_) => {
                stop.store(true, Ordering::SeqCst);
                Err("The microphone did not start.".to_string())
            }
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Why the computer's sound is not being recorded, when it was asked for.
    pub fn system_audio_problem(&self) -> Option<&str> {
        self.system_audio_problem.as_deref()
    }

    pub fn stop(self) -> Result<RecordingSummary, String> {
        self.stop.store(true, Ordering::SeqCst);
        self.worker
            .join()
            .map_err(|_| "The recorder stopped unexpectedly.".to_string())?
    }
}

/// A gap this long between what a source has delivered and the clock is real, not jitter.
const TOLERANCE: u64 = TARGET_RATE as u64 / 4;
/// A source this far behind has gone quiet and must not hold the other one back.
const STALL: u64 = TARGET_RATE as u64;

/// One channel of the file, kept on the recording's clock. The two sources run on clocks of
/// their own and either can pause (a device that changes, a tap with nothing to deliver), so
/// each is checked against the time that has really passed and silence fills what is missing.
/// That keeps the channels lined up, which the echo handling later depends on.
#[derive(Default)]
struct Track {
    queue: VecDeque<f32>,
    produced: u64,
}

impl Track {
    /// `now` is the recording's age in samples. Call once per round with everything that
    /// arrived, so a burst after a hiccup is not mistaken for a gap.
    fn push(&mut self, samples: &[f32], now: u64) {
        let end = self.produced + samples.len() as u64;
        if now > end + TOLERANCE {
            self.silence(now - end);
        }
        self.queue.extend(samples);
        self.produced += samples.len() as u64;
    }

    fn keep_up(&mut self, now: u64) {
        if now > self.produced + STALL {
            self.silence(now - self.produced - STALL / 2);
        }
    }

    fn silence(&mut self, count: u64) {
        self.queue.extend(std::iter::repeat_n(0.0, count as usize));
        self.produced += count;
    }
}

/// Writes every frame all tracks have; with `finish`, what is left of any of them too.
fn write_frames(
    tracks: &mut [&mut Track],
    finish: bool,
    mut write: impl FnMut(f32) -> Result<(), String>,
) -> Result<u64, String> {
    let lengths = tracks.iter().map(|track| track.queue.len());
    let frames = if finish { lengths.max() } else { lengths.min() }.unwrap_or(0);
    for _ in 0..frames {
        for track in tracks.iter_mut() {
            write(track.queue.pop_front().unwrap_or(0.0))?;
        }
    }
    Ok(frames as u64)
}

/// A stream of blocks on its way to a track: down to one channel, then to 16 kHz.
struct Source {
    blocks: mpsc::Receiver<Vec<f32>>,
    channels: usize,
    resampler: Resampler,
    track: Track,
    resampled: Vec<f32>,
}

impl Source {
    fn new(blocks: mpsc::Receiver<Vec<f32>>, rate: u32, channels: usize) -> Self {
        Self {
            blocks,
            channels,
            resampler: Resampler::new(rate),
            track: Track::default(),
            resampled: Vec::new(),
        }
    }

    /// Moves `first` and everything else that has arrived onto the track, and returns the
    /// level of the loudest block.
    fn take(&mut self, first: Option<Vec<f32>>, now: u64) -> f32 {
        let mut level = 0.0f32;
        self.resampled.clear();
        for block in first.into_iter().chain(self.blocks.try_iter()) {
            let mono = downmix(&block, self.channels);
            level = level.max(rms(&mono));
            self.resampler.process(&mono, &mut self.resampled);
        }
        if !self.resampled.is_empty() {
            self.track.push(&self.resampled, now);
        }
        self.track.keep_up(now);
        level
    }
}

fn record(
    path: &Path,
    system_audio: bool,
    stop: &AtomicBool,
    ready: mpsc::Sender<Result<Option<String>, String>>,
    on_level: impl Fn(f32),
) -> Result<RecordingSummary, String> {
    let (stream, source_rate, channels, blocks) = match open_input() {
        Ok(parts) => parts,
        Err(error) => {
            let _ = ready.send(Err(error.clone()));
            return Err(error);
        }
    };
    let mut microphone = Source::new(blocks, source_rate, channels);

    // The far side of a call is worth having, but never worth losing the meeting for.
    let mut system_audio_problem = None;
    let mut system = None;
    if system_audio {
        match SystemAudio::start() {
            Ok((capture, stream)) => {
                system = Some((capture, Source::new(stream.blocks, stream.rate, stream.channels)));
            }
            Err(error) => {
                tracing::warn!(%error, "system_audio_unavailable");
                system_audio_problem = Some(error);
            }
        }
    }

    let spec = hound::WavSpec {
        channels: if system.is_some() { 2 } else { 1 },
        sample_rate: TARGET_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = match hound::WavWriter::create(path, spec) {
        Ok(writer) => writer,
        Err(error) => {
            let error = format!("Could not create {}: {error}", path.display());
            let _ = ready.send(Err(error.clone()));
            return Err(error);
        }
    };

    // Both start from the same moment: what arrived while the other was being set up goes.
    microphone.blocks.try_iter().for_each(drop);
    if let Some((_, system)) = &system {
        system.blocks.try_iter().for_each(drop);
    }
    let started = Instant::now();
    let _ = ready.send(Ok(system_audio_problem));

    let mut written = 0u64;
    let mut last_level = Instant::now();
    let mut last_flush = Instant::now();
    let mut peak_level = 0.0f32;

    loop {
        let stopping = stop.load(Ordering::SeqCst);
        let received = microphone.blocks.recv_timeout(Duration::from_millis(50));
        let now = (started.elapsed().as_secs_f64() * TARGET_RATE as f64) as u64;
        let done = match received {
            Ok(block) => {
                peak_level = peak_level.max(microphone.take(Some(block), now));
                false
            }
            // Nothing left in the queue after the stop request: done.
            Err(mpsc::RecvTimeoutError::Timeout) if stopping => true,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                microphone.take(None, now);
                false
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => true,
        };
        if let Some((_, system)) = &mut system {
            peak_level = peak_level.max(system.take(None, now));
        }

        let mut tracks = vec![&mut microphone.track];
        tracks.extend(system.as_mut().map(|(_, system)| &mut system.track));
        written += write_frames(&mut tracks, done, |sample| {
            let value = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            writer.write_sample(value).map_err(|e| e.to_string())
        })?;
        if done {
            break;
        }

        if last_level.elapsed() >= LEVEL_INTERVAL {
            on_level(peak_level);
            peak_level = 0.0;
            last_level = Instant::now();
        }
        if last_flush.elapsed() >= FLUSH_INTERVAL {
            writer.flush().map_err(|e| e.to_string())?;
            last_flush = Instant::now();
        }
        if stopping {
            // Stop the device, then drain what it already delivered.
            let _ = stream.pause();
        }
    }

    drop(stream);
    let system_audio = system.is_some();
    drop(system);
    writer.finalize().map_err(|e| e.to_string())?;

    Ok(RecordingSummary {
        path: path.to_path_buf(),
        duration_seconds: written as f64 / TARGET_RATE as f64,
        system_audio,
    })
}

type Input = (cpal::Stream, u32, usize, mpsc::Receiver<Vec<f32>>);

fn open_input() -> Result<Input, String> {
    let device = cpal::default_host()
        .default_input_device()
        .ok_or_else(|| "No microphone was found.".to_string())?;
    let supported = device
        .default_input_config()
        .map_err(|e| format!("The microphone could not be opened: {e}"))?;
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let (tx, rx) = mpsc::channel::<Vec<f32>>();
    let on_error = |error| tracing::warn!(%error, "microphone_stream_error");

    let stream = match format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let _ = tx.send(data.to_vec());
            },
            on_error,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                let _ = tx.send(data.iter().map(|s| *s as f32 / 32_768.0).collect());
            },
            on_error,
            None,
        ),
        cpal::SampleFormat::I32 => device.build_input_stream(
            &config,
            move |data: &[i32], _: &cpal::InputCallbackInfo| {
                let _ = tx.send(data.iter().map(|s| *s as f32 / 2_147_483_648.0).collect());
            },
            on_error,
            None,
        ),
        other => return Err(format!("The microphone's sample format ({other}) is not supported.")),
    }
    .map_err(|e| format!("The microphone could not be opened: {e}"))?;

    stream
        .play()
        .map_err(|e| format!("The microphone could not be started: {e}"))?;

    Ok((stream, config.sample_rate, config.channels as usize, rx))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: u64 = TARGET_RATE as u64;

    fn written(tracks: &mut [&mut Track], finish: bool) -> Vec<f32> {
        let mut samples = Vec::new();
        write_frames(tracks, finish, |sample| {
            samples.push(sample);
            Ok(())
        })
        .unwrap();
        samples
    }

    #[test]
    fn frames_wait_for_both_channels_and_interleave_them() {
        let (mut microphone, mut system) = (Track::default(), Track::default());
        microphone.push(&[0.1, 0.2, 0.3], 3);
        system.push(&[0.5, 0.6], 3);

        assert_eq!(written(&mut [&mut microphone, &mut system], false), [0.1, 0.5, 0.2, 0.6]);
        // Stopping writes the rest, with silence where a channel has run out.
        assert_eq!(written(&mut [&mut microphone, &mut system], true), [0.3, 0.0]);
    }

    #[test]
    fn a_source_that_paused_comes_back_at_the_right_time() {
        let mut track = Track::default();
        track.push(&[0.5; 160], 200);
        // Nothing for two seconds, then sound again.
        track.push(&[0.7; 160], 2 * SECOND + 200);

        assert_eq!(track.produced, 2 * SECOND + 200);
        let samples = track.queue.iter().copied().collect::<Vec<_>>();
        assert_eq!(samples[159], 0.5);
        assert!(samples[160..samples.len() - 160].iter().all(|sample| *sample == 0.0));
        assert_eq!(samples[samples.len() - 160], 0.7);
    }

    #[test]
    fn ordinary_lateness_is_not_padded() {
        let mut track = Track::default();
        let mut now = 0;
        for _ in 0..100 {
            now += 160;
            // Blocks show up 100 ms after they were captured.
            track.push(&[0.5; 160], now + SECOND / 10);
        }

        assert_eq!(track.produced, 100 * 160);
        assert!(track.queue.iter().all(|sample| *sample == 0.5));
    }

    #[test]
    fn a_silent_source_does_not_hold_the_recording_back() {
        let (mut microphone, mut system) = (Track::default(), Track::default());
        let mut frames = 0;
        for round in 1..=300u64 {
            let now = round * 160;
            microphone.push(&[0.5; 160], now);
            system.keep_up(now);
            frames += written(&mut [&mut microphone, &mut system], false).len() as u64 / 2;
        }

        // Three seconds in, all but the last second is on disk.
        assert!(frames >= 2 * SECOND && frames <= 3 * SECOND, "{frames}");
        assert!(microphone.queue.len() as u64 <= SECOND);
    }

    /// Plays something shaped like talking through the loudspeakers while recording both
    /// channels. Needs a microphone, the microphone and System Audio Recording permissions
    /// for whatever runs the test, and the volume up:
    ///
    /// cargo test -p engine live_recording -- --ignored --nocapture
    #[test]
    #[ignore = "plays noise out loud, and needs the microphone and System Audio Recording permissions"]
    fn live_recording_has_the_microphone_and_the_computer_side_by_side() {
        let dir = tempfile::tempdir().unwrap();
        let played = dir.path().join("played.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: TARGET_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&played, spec).unwrap();
        for sample in crate::audio::test_signals::talk(8, &[(0.5, 7.5)], 0.5, 11) {
            writer.write_sample((sample * i16::MAX as f32) as i16).unwrap();
        }
        writer.finalize().unwrap();

        let path = dir.path().join("audio.wav");
        let recording = Recording::start(path.clone(), true, |_| {}).expect("a recording");
        assert_eq!(recording.system_audio_problem(), None);
        let player = std::process::Command::new("afplay").args(["-v", "0.3"]).arg(&played).status();
        assert!(player.expect("afplay").success());
        let wall_clock = recording.elapsed().as_secs_f64();
        let summary = recording.stop().expect("a summary");

        let channels = crate::audio::read_wav_16k_channels(&path).unwrap();
        let levels = channels.iter().map(|channel| rms(channel)).collect::<Vec<_>>();
        let frames = |channel: &[f32]| channel.chunks(480).map(rms).collect::<Vec<_>>();
        let echo = crate::mixdown::find_echo(&frames(&channels[0]), &frames(&channels[1]));
        println!(
            "{wall_clock:.2} s on the clock, {:.2} s in the file, levels {levels:?}, {echo:?}",
            summary.duration_seconds
        );

        assert!(summary.system_audio);
        assert_eq!(channels.len(), 2);
        assert!((summary.duration_seconds - wall_clock).abs() < 0.5);
        assert!(levels[1] > 0.005, "the computer's channel is silent");

        // On loudspeakers the microphone hears it all again; the mix must not.
        if echo.is_some() {
            let mixed = crate::mixdown::mix_call(&channels[0], &channels[1]);
            let left = mixed.iter().zip(&channels[1]).map(|(mixed, system)| mixed - system).collect::<Vec<_>>();
            println!("echo in the microphone {:.4}, left in the mix {:.4}", levels[0], rms(&left));
            assert!(rms(&left) < 0.2 * levels[0]);
        }
    }
}
