//! Records the default microphone to a 16 kHz mono WAV file.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::audio::{Resampler, TARGET_RATE, downmix, rms};

const LEVEL_INTERVAL: Duration = Duration::from_millis(100);
/// How often the WAV header is brought up to date, so a crash loses seconds, not the file.
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct RecordingSummary {
    pub path: PathBuf,
    pub duration_seconds: f64,
}

pub struct Recording {
    stop: Arc<AtomicBool>,
    worker: JoinHandle<Result<RecordingSummary, String>>,
    started: Instant,
}

impl Recording {
    /// Starts recording and returns once audio is flowing. `on_level` gets the input
    /// level (RMS, 0 to 1) about ten times a second.
    pub fn start(
        path: PathBuf,
        on_level: impl Fn(f32) + Send + 'static,
    ) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel();

        // The stream is not `Send` on macOS, so it lives and dies on this thread.
        let worker = std::thread::Builder::new()
            .name("recorder".to_string())
            .spawn({
                let stop = stop.clone();
                move || record(&path, &stop, ready_tx, on_level)
            })
            .map_err(|e| e.to_string())?;

        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(Self {
                stop,
                worker,
                started: Instant::now(),
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

    pub fn stop(self) -> Result<RecordingSummary, String> {
        self.stop.store(true, Ordering::SeqCst);
        self.worker
            .join()
            .map_err(|_| "The recorder stopped unexpectedly.".to_string())?
    }
}

fn record(
    path: &Path,
    stop: &AtomicBool,
    ready: mpsc::Sender<Result<(), String>>,
    on_level: impl Fn(f32),
) -> Result<RecordingSummary, String> {
    let started = open_input();
    let (stream, source_rate, channels, blocks) = match started {
        Ok(parts) => parts,
        Err(error) => {
            let _ = ready.send(Err(error.clone()));
            return Err(error);
        }
    };

    let spec = hound::WavSpec {
        channels: 1,
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
    let _ = ready.send(Ok(()));

    let mut resampler = Resampler::new(source_rate);
    let mut resampled = Vec::new();
    let mut written = 0u64;
    let mut last_level = Instant::now();
    let mut last_flush = Instant::now();
    let mut peak_level = 0.0f32;

    loop {
        let stopping = stop.load(Ordering::SeqCst);
        match blocks.recv_timeout(Duration::from_millis(50)) {
            Ok(block) => {
                let mono = downmix(&block, channels);
                peak_level = peak_level.max(rms(&mono));

                resampled.clear();
                resampler.process(&mono, &mut resampled);
                for sample in &resampled {
                    let value = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                    writer.write_sample(value).map_err(|e| e.to_string())?;
                }
                written += resampled.len() as u64;
            }
            // Nothing left in the queue after the stop request: done.
            Err(mpsc::RecvTimeoutError::Timeout) if stopping => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
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
    writer.finalize().map_err(|e| e.to_string())?;

    Ok(RecordingSummary {
        path: path.to_path_buf(),
        duration_seconds: written as f64 / TARGET_RATE as f64,
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
