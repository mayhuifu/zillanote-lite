//! Brings a recording made elsewhere into a meeting's folder as 16 kHz mono WAV, the form
//! everything downstream reads. The original file is only ever read.

use std::path::Path;

use crate::audio::{TARGET_RATE, read_wav_16k_mono};

/// What the file dialog offers. WAV is read directly; the rest goes through macOS.
pub const EXTENSIONS: &[&str] = &["wav", "m4a", "mp3", "mp4", "mov", "aac", "aiff", "aif", "caf", "flac"];

/// Returns the recording's length in seconds.
pub fn import_audio(source: &Path, target: &Path) -> Result<f64, String> {
    if !source.is_file() {
        return Err(format!("{} is not a file.", source.display()));
    }
    // A WAV with something other than plain samples inside goes to the converter too.
    let samples = match read_wav_16k_mono(source) {
        Ok(samples) => samples,
        Err(_) => {
            // The converter keeps the channels: its own mix to one makes two alike channels
            // 3 dB louder, enough to clip a loud recording. They are averaged here instead.
            let converted = target.with_extension("converted.wav");
            let samples = convert(source, &converted).and_then(|()| read_wav_16k_mono(&converted));
            let _ = std::fs::remove_file(&converted);
            samples?
        }
    };

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(target, spec).map_err(|e| format!("{}: {e}", target.display()))?;
    for sample in &samples {
        let value = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        writer.write_sample(value).map_err(|e| e.to_string())?;
    }
    writer.finalize().map_err(|e| e.to_string())?;
    Ok(seconds(&samples))
}

fn seconds(samples: &[f32]) -> f64 {
    samples.len() as f64 / TARGET_RATE as f64
}

/// `afconvert` ships with macOS and reads whatever Core Audio reads, the sound track of a
/// video included.
#[cfg(target_os = "macos")]
fn convert(source: &Path, target: &Path) -> Result<(), String> {
    let output = std::process::Command::new("/usr/bin/afconvert")
        .args(["-f", "WAVE", "-d", "LEI16@16000"])
        .arg(source)
        .arg(target)
        .output()
        .map_err(|e| format!("The recording could not be converted: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let _ = std::fs::remove_file(target);
    let reason = String::from_utf8_lossy(&output.stderr);
    Err(format!(
        "{} is not a recording macOS can read. {}",
        source.file_name().unwrap_or_default().to_string_lossy(),
        reason.lines().last().unwrap_or_default().trim()
    ))
}

#[cfg(not(target_os = "macos"))]
fn convert(_: &Path, _: &Path) -> Result<(), String> {
    Err("Only WAV recordings can be imported on this system so far.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::rms;

    /// One second of a 500 Hz tone, stereo at 48 kHz.
    fn recording(path: &Path) {
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..48_000 {
            let value = ((2.0 * std::f32::consts::PI * 500.0 * i as f32 / 48_000.0).sin() * 16_000.0) as i16;
            writer.write_sample(value).unwrap();
            writer.write_sample(value).unwrap();
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn a_wav_of_any_shape_arrives_as_16k_mono_and_the_original_stays() {
        let dir = tempfile::tempdir().unwrap();
        let (source, target) = (dir.path().join("teams call.wav"), dir.path().join("audio.wav"));
        recording(&source);
        let before = std::fs::read(&source).unwrap();

        let seconds = import_audio(&source, &target).unwrap();

        assert!((seconds - 1.0).abs() < 0.01);
        let spec = hound::WavReader::open(&target).unwrap().spec();
        assert_eq!((spec.channels, spec.sample_rate), (1, TARGET_RATE));
        assert_eq!(std::fs::read(&source).unwrap(), before);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn compressed_recordings_go_through_macos() {
        let dir = tempfile::tempdir().unwrap();
        let (wav, m4a, target) = (dir.path().join("in.wav"), dir.path().join("memo.m4a"), dir.path().join("audio.wav"));
        recording(&wav);
        let encoded = std::process::Command::new("/usr/bin/afconvert")
            .args(["-f", "m4af", "-d", "aac"])
            .arg(&wav)
            .arg(&m4a)
            .status()
            .unwrap();
        assert!(encoded.success());

        let seconds = import_audio(&m4a, &target).unwrap();

        assert!((seconds - 1.0).abs() < 0.1, "{seconds}");
        let samples = read_wav_16k_mono(&target).unwrap();
        assert!((rms(&samples[2_000..14_000]) - 0.345).abs() < 0.05);
    }

    #[test]
    fn something_that_is_not_a_recording_is_refused_and_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (source, target) = (dir.path().join("notes.m4a"), dir.path().join("audio.wav"));
        std::fs::write(&source, b"these are not samples").unwrap();

        assert!(import_audio(&source, &target).is_err());
        assert!(import_audio(&dir.path().join("missing.wav"), &target).is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "only the source is left");
    }
}
