//! Tests that run the real models. They need the two ONNX files and a recording:
//!
//! ZILLANOTE_SPEAKER_MODELS=<folder with segmentation.onnx and embedding.onnx> \
//! ZILLANOTE_TEST_AUDIO=<a WAV with two or three people talking> \
//! cargo test -p speakers --test live -- --ignored --nocapture

use std::path::PathBuf;

use speakers::segmentation::{Segmenter, WINDOW_SAMPLES, frames_for_samples};
use speakers::{DiarizationConfig, DiarizeRequest, Diarizer, KnownSpeaker, SAMPLE_RATE, SpeakerBounds, SpeakerModels};

fn models() -> SpeakerModels {
    let dir = PathBuf::from(std::env::var("ZILLANOTE_SPEAKER_MODELS").expect("ZILLANOTE_SPEAKER_MODELS"));
    SpeakerModels::locate(&dir).expect("segmentation.onnx and embedding.onnx")
}

fn audio() -> Vec<f32> {
    let path = std::env::var("ZILLANOTE_TEST_AUDIO").expect("ZILLANOTE_TEST_AUDIO");
    let mut reader = hound::WavReader::open(path).expect("a WAV file");
    let spec = reader.spec();
    assert_eq!((spec.sample_rate, spec.channels), (SAMPLE_RATE, 1), "16 kHz mono, please");
    reader.samples::<i16>().map(|sample| sample.unwrap() as f32 / 32_768.0).collect()
}

fn diarizer() -> Diarizer {
    Diarizer::new(&models(), DiarizationConfig::default()).unwrap()
}

#[test]
#[ignore = "needs the speaker models and a recording"]
fn live_window_produces_frames_with_speech_and_silence_none() {
    let mut window = audio();
    window.resize(WINDOW_SAMPLES, 0.0);
    let mut segmenter = Segmenter::new(&models().segmentation).unwrap();

    let activity = segmenter.run_window(&window).unwrap();
    assert_eq!(activity.frames.len(), frames_for_samples(WINDOW_SAMPLES));
    assert!(activity.speech_frames() > 0);

    let silence = segmenter.run_window(&vec![0.0; WINDOW_SAMPLES]).unwrap();
    assert_eq!(silence.speech_frames(), 0);
    assert!(segmenter.run_window(&[0.0; 10]).is_err());
}

#[test]
#[ignore = "needs the speaker models and a recording"]
fn live_diarization_finds_the_speakers_and_names_known_ones() {
    let audio = audio();
    let mut diarizer = diarizer();
    let started = std::time::Instant::now();
    let first = diarizer.diarize(&mut audio.as_slice(), &DiarizeRequest::default()).unwrap();
    let duration = audio.len() as f64 / SAMPLE_RATE as f64;
    println!(
        "{duration:.0} s of audio in {:?}: {} speakers, {} turns",
        started.elapsed(),
        first.speaker_count(),
        first.segments.len()
    );

    assert!(!first.segments.is_empty());
    assert!((1..=3).contains(&first.speaker_count()));
    assert!(first.segments.windows(2).all(|pair| pair[0].start <= pair[1].start));
    assert!(first.segments.iter().all(|segment| segment.end <= duration + 1e-6));

    // Two exemplars per person, as a voice named twice would have.
    let known = first
        .speakers
        .iter()
        .flat_map(|speaker| {
            [1.0f32, 0.9].map(|scale| KnownSpeaker {
                id: format!("human-{}", speaker.index),
                embedding: speaker.centroid.iter().map(|value| value * scale).collect(),
            })
        })
        .collect::<Vec<_>>();
    let request = DiarizeRequest {
        known_speakers: &known,
        ..DiarizeRequest::default()
    };
    let second = diarizer.diarize(&mut audio.as_slice(), &request).unwrap();

    assert_eq!(second.speaker_count(), first.speaker_count());
    for speaker in &second.speakers {
        let identity = speaker.identity.as_ref().expect("a centroid matches itself");
        assert_eq!(identity.id, format!("human-{}", speaker.index));
    }
}

#[test]
#[ignore = "needs the speaker models and a recording"]
fn live_speaker_count_and_progress_are_respected() {
    let audio = audio();
    let seen = std::cell::RefCell::new(Vec::new());
    let on_progress = |fraction: f32| seen.borrow_mut().push(fraction);
    let request = DiarizeRequest {
        bounds: SpeakerBounds::exact(2),
        on_progress: Some(&on_progress),
        ..DiarizeRequest::default()
    };

    let diarization = diarizer().diarize(&mut audio.as_slice(), &request).unwrap();

    assert_eq!(diarization.speaker_count(), 2);
    let seen = seen.into_inner();
    assert!(seen.len() > 1 && seen.windows(2).all(|pair| pair[0] < pair[1]));
    assert!((seen.last().unwrap() - 1.0).abs() < f32::EPSILON);
}

#[test]
#[ignore = "needs the speaker models"]
fn live_silence_yields_nothing() {
    let audio = vec![0.0f32; SAMPLE_RATE as usize * 12];

    let diarization = diarizer().diarize(&mut audio.as_slice(), &DiarizeRequest::default()).unwrap();

    assert!(diarization.segments.is_empty());
    assert_eq!(diarization.speaker_count(), 0);
}
