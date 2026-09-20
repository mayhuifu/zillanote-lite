//! What happens after Stop: transcribe, then write minutes, one model in memory at a time.

use std::cell::RefCell;
use std::path::PathBuf;

use qwen3_asr::{
    LlamaServer, LlamaServerConfig, Qwen3AsrClient, Qwen3AsrModel, describe_missing_binary, find_llama_server,
    kill_stale_servers,
};

use speakers::{DiarizationConfig, DiarizeRequest, Diarizer, SpeakerModels};

use crate::audio::{TARGET_RATE, read_wav_16k_channels};
use crate::chunker::{Chunk, ChunkerConfig, speech_chunks};
use crate::email;
use crate::minutes::{About, LlmConfig, write_minutes};
use crate::mixdown::recognition_signal;
use crate::store::{Meeting, Settings, Status, Store};
use crate::templates::template;
use crate::transcript::{Segment, Transcript};
use crate::turns::{Piece, Turn, seconds_by_speaker, split_at_turns};
use crate::voices::{self, MeetingSpeaker};

/// For when the app leaves: a recognizer still running would keep gigabytes of memory.
pub fn stop_servers() {
    let _ = kill_stale_servers();
}

/// How the error of a meeting that waits for the speech model starts. Such meetings are
/// processed by themselves once the model has been downloaded.
pub const MODEL_MISSING: &str = "The speech model is not on this Mac yet.";

/// How much of the "transcribing" progress bar finding the speakers gets.
const SPEAKERS_SHARE: f32 = 0.25;

#[derive(Debug, Clone)]
pub struct Pipeline {
    pub store: Store,
    /// Folders a `llama-server` shipped with the app may be in.
    pub bundled_server_dirs: Vec<PathBuf>,
}

impl Pipeline {
    /// Transcribes the meeting's recording, then writes its minutes. `on_update` sees every
    /// change of status and progress. A failure is recorded on the meeting, not returned.
    pub async fn process(&self, id: &str, on_update: &(dyn Fn(&Meeting) + Send + Sync)) {
        let Ok(mut meeting) = self.store.meeting(id) else {
            return;
        };

        if let Err(error) = self.transcribe(&mut meeting, on_update).await {
            self.fail(&mut meeting, error, on_update);
            return;
        }
        // The minutes follow by themselves, unless the user would rather ask for them.
        if self.store.settings().auto_minutes {
            self.summarize(&mut meeting, on_update).await;
        } else {
            self.update(&mut meeting, Status::Done, 1.0, on_update);
        }
    }

    /// Makes a meeting from a recording made elsewhere, ready for [`Pipeline::process`]. The
    /// file is copied in as 16 kHz WAV; the original is left alone.
    pub fn import(&self, source: &std::path::Path) -> Result<Meeting, String> {
        let template = self.store.settings().default_template;
        let mut meeting = self.store.create_meeting(chrono::Local::now(), &template)?;
        match crate::import::import_audio(source, &self.store.audio_path(&meeting.id)) {
            Ok(seconds) => {
                if let Some(name) = source.file_stem() {
                    meeting.title = name.to_string_lossy().into_owned();
                }
                meeting.duration_seconds = seconds;
                meeting.status = Status::Transcribing;
                self.store.save_meeting(&meeting)?;
                Ok(meeting)
            }
            Err(error) => {
                let _ = self.store.delete_meeting(&meeting.id);
                Err(error)
            }
        }
    }

    /// Writes the minutes again from the existing transcript, with another template.
    pub async fn rewrite_minutes(
        &self,
        id: &str,
        template_id: &str,
        on_update: &(dyn Fn(&Meeting) + Send + Sync),
    ) {
        let Ok(mut meeting) = self.store.meeting(id) else {
            return;
        };
        meeting.template = template(template_id).id.to_string();
        self.summarize(&mut meeting, on_update).await;
    }

    async fn transcribe(
        &self,
        meeting: &mut Meeting,
        on_update: &(dyn Fn(&Meeting) + Send + Sync),
    ) -> Result<(), String> {
        self.update(meeting, Status::Transcribing, 0.0, on_update);
        // Before any work: finding the speakers takes minutes, and would be for nothing.
        let server_config = self.server_config()?;

        let audio_path = self.store.audio_path(&meeting.id);
        let samples = tokio::task::spawn_blocking(move || read_wav_16k_channels(&audio_path).map(recognition_signal))
            .await
            .map_err(|e| e.to_string())??;
        let duration_seconds = samples.len() as f64 / TARGET_RATE as f64;
        let chunks = speech_chunks(&samples, TARGET_RATE, ChunkerConfig::default());
        if chunks.is_empty() {
            return Err("No speech was found in the recording.".to_string());
        }

        // Who spoke when comes first: its models are gone again before the recognizer's load.
        let (pieces, speakers) = self.find_speakers(meeting, &samples, &chunks, on_update);
        let share = if speakers.is_empty() { 0.0 } else { SPEAKERS_SHARE };

        let settings = self.store.settings();
        let model = settings.asr_model;
        let mut server = self.start_server(server_config).await?;
        let client = Qwen3AsrClient::new(&server.base_url(), model)
            .map_err(|e| e.to_string())?
            .with_vocabulary(&settings.vocabulary);

        let mut segments = Vec::new();
        let mut failures = 0;
        let mut last_error = String::new();
        for (index, piece) in pieces.iter().enumerate() {
            match client.transcribe_samples(&samples[piece.start..piece.end]).await {
                Ok(text) if !text.trim().is_empty() => segments.push(Segment {
                    start: piece.start as f64 / TARGET_RATE as f64,
                    end: piece.end as f64 / TARGET_RATE as f64,
                    text: text.trim().to_string(),
                    speaker: piece.speaker.and_then(|index| Some(speakers.get(index)?.label.clone())),
                    speaker_index: piece.speaker,
                }),
                Ok(_) => {}
                Err(error) => {
                    failures += 1;
                    last_error = error.to_string();
                    tracing::warn!(chunk = index, error = %last_error, "chunk_failed");
                }
            }
            self.update(
                meeting,
                Status::Transcribing,
                share + (1.0 - share) * (index + 1) as f32 / pieces.len() as f32,
                on_update,
            );
        }
        // The recognizer's memory goes back before the language model is asked for any.
        server.stop().await;

        // A few lost chunks leave gaps; every chunk failing means the server is unusable.
        if segments.is_empty() && failures > 0 {
            return Err(format!("Transcription failed: {last_error}"));
        }

        let transcript = Transcript {
            duration_seconds,
            engine: model.as_str().to_string(),
            segments,
        };
        if transcript.is_empty() {
            return Err("No speech was recognized in the recording.".to_string());
        }
        self.store.save_transcript(&meeting.id, &transcript)?;
        self.store.save_speakers(&meeting.id, &speakers)?;
        tracing::info!(
            meeting = %meeting.id,
            seconds = duration_seconds as u64,
            segments = transcript.segments.len(),
            failed_chunks = failures,
            "transcribed"
        );
        meeting.duration_seconds = duration_seconds;
        meeting.has_transcript = true;
        Ok(())
    }

    /// Cuts the chunks at the changes of speaker and says who the speakers are, recognizing
    /// the voices the user has named. Best effort: without the speaker models, or if they
    /// fail, the chunks come back as they are and the transcript has no names.
    fn find_speakers(
        &self,
        meeting: &Meeting,
        samples: &[f32],
        chunks: &[Chunk],
        on_update: &(dyn Fn(&Meeting) + Send + Sync),
    ) -> (Vec<Piece>, Vec<MeetingSpeaker>) {
        let unnamed = || {
            let pieces = chunks.iter().map(|chunk| Piece {
                start: chunk.start,
                end: chunk.end,
                speaker: None,
            });
            (pieces.collect(), Vec::new())
        };
        let Some(models) = SpeakerModels::locate(&self.store.speaker_models_dir()) else {
            return unnamed();
        };

        let voices = self.store.voices();
        let known = voices::known_speakers(&voices);
        let shown = RefCell::new(meeting.clone());
        let on_progress = |fraction: f32| {
            let mut shown = shown.borrow_mut();
            let progress = SPEAKERS_SHARE * fraction;
            if progress - shown.progress >= 0.01 {
                shown.progress = progress;
                let _ = self.store.save_meeting(&shown);
                on_update(&shown);
            }
        };
        // Minutes of arithmetic: tell the runtime this thread is busy.
        let result = tokio::task::block_in_place(|| {
            // Full detail up to forty minutes; longer recordings are looked at in wider steps,
            // which keeps the wait for a two-hour meeting near that of a one-hour one.
            let config = DiarizationConfig {
                max_windows: 1_200,
                ..DiarizationConfig::default()
            };
            let mut diarizer = Diarizer::new(&models, config)?;
            let request = DiarizeRequest {
                known_speakers: &known,
                on_progress: Some(&on_progress),
                ..DiarizeRequest::default()
            };
            let mut audio = samples;
            diarizer.diarize(&mut audio, &request)
        });
        let diarization = match result {
            Ok(diarization) => diarization,
            Err(error) => {
                tracing::warn!(meeting = %meeting.id, %error, "speakers_not_found");
                return unnamed();
            }
        };

        let turns = diarization
            .segments
            .iter()
            .map(|segment| Turn {
                start: segment.start,
                end: segment.end,
                speaker: segment.speaker,
            })
            .collect::<Vec<_>>();
        let seconds = seconds_by_speaker(&turns);
        let speakers = diarization
            .speakers
            .into_iter()
            .map(|speaker| {
                let voice = speaker
                    .identity
                    .and_then(|identity| voices.iter().find(|voice| voice.id == identity.id));
                MeetingSpeaker {
                    index: speaker.index,
                    label: voice.map_or_else(|| voices::default_label(speaker.index), |voice| voice.name.clone()),
                    voice_id: voice.map(|voice| voice.id.clone()),
                    seconds: seconds.get(speaker.index).copied().unwrap_or(0.0),
                    centroid: speaker.centroid,
                }
            })
            .collect::<Vec<_>>();
        tracing::info!(meeting = %meeting.id, speakers = speakers.len(), turns = turns.len(), "speakers_found");

        (split_at_turns(chunks, &turns, samples, TARGET_RATE), speakers)
    }

    /// Puts a name to one of a meeting's speakers and remembers the voice under it; an
    /// empty name takes the name off again. The transcript's lines follow.
    pub fn name_speaker(&self, id: &str, index: usize, name: &str) -> Result<(), String> {
        let mut speakers = self.store.speakers(id);
        let speaker = speakers
            .iter_mut()
            .find(|speaker| speaker.index == index)
            .ok_or_else(|| "This meeting has no such speaker.".to_string())?;
        let mut voices = self.store.voices();
        if name.trim().is_empty() {
            voices::unname_speaker(&mut voices, speaker);
        } else {
            voices::name_speaker(&mut voices, speaker, name, || {
                format!("voice-{}", chrono::Local::now().timestamp_millis())
            });
        }
        let label = speaker.label.clone();

        self.store.save_voices(&voices)?;
        self.store.save_speakers(id, &speakers)?;
        if let Some(mut transcript) = self.store.transcript(id) {
            for segment in &mut transcript.segments {
                if segment.speaker_index == Some(index) {
                    segment.speaker = Some(label.clone());
                }
            }
            self.store.save_transcript(id, &transcript)?;
        }
        Ok(())
    }

    /// Forgets a named voice. Meetings keep the name where it already stands.
    pub fn forget_voice(&self, voice_id: &str) -> Result<(), String> {
        let mut voices = self.store.voices();
        voices.retain(|voice| voice.id != voice_id);
        self.store.save_voices(&voices)
    }

    /// What the recognizer needs to start, or what is missing.
    fn server_config(&self) -> Result<LlamaServerConfig, String> {
        let files = self.store.settings().asr_model.locate_files(&self.store.models_dir()).ok_or_else(|| {
            format!("{MODEL_MISSING} Download it on the home view: this recording is transcribed as soon as it is here.")
        })?;
        let binary = find_llama_server(&self.bundled_server_dirs).ok_or_else(describe_missing_binary)?;
        Ok(LlamaServerConfig::new(binary, files))
    }

    async fn start_server(&self, config: LlamaServerConfig) -> Result<LlamaServer, String> {
        // A run of the app that crashed leaves its server behind, holding the model in memory.
        let _ = tokio::task::spawn_blocking(kill_stale_servers).await;
        LlamaServer::start(config).await.map_err(|e| e.to_string())
    }

    /// The meetings that failed only because the speech model was not there yet.
    pub fn waiting_for_model(&self) -> Vec<String> {
        let meetings = self.store.meetings().into_iter();
        meetings
            .filter(|meeting| meeting.status == Status::Failed)
            .filter(|meeting| meeting.error.as_deref().is_some_and(|error| error.starts_with(MODEL_MISSING)))
            .map(|meeting| meeting.id)
            .collect()
    }

    async fn summarize(&self, meeting: &mut Meeting, on_update: &(dyn Fn(&Meeting) + Send + Sync)) {
        let Some(transcript) = self.store.transcript(&meeting.id) else {
            self.fail(meeting, "There is no transcript to write minutes from.".to_string(), on_update);
            return;
        };
        self.update(meeting, Status::Summarizing, 0.0, on_update);

        let settings = self.store.settings();
        // With the weekday, so "on Tuesday" in the meeting can become a date in the minutes.
        let date = chrono::DateTime::parse_from_rfc3339(&meeting.created_at)
            .map(|at| at.format("%Y-%m-%d (%A)").to_string())
            .unwrap_or_default();
        let about = About {
            title: &meeting.title,
            date: &date,
            known_terms: &settings.vocabulary,
        };
        let result = write_minutes(
            &llm_config(&settings),
            &settings.system_prompt,
            template(&meeting.template),
            about,
            &transcript.to_text(),
            |_, _| {},
        )
        .await
        .and_then(|minutes| self.store.save_minutes(&meeting.id, &minutes));

        // The transcript is the hard part and it is safe: a meeting without minutes is
        // still done, with the reason shown and "write minutes" one click away.
        meeting.has_minutes = self.store.minutes(&meeting.id).is_some();
        let written = result.is_ok();
        // In the log too: the message on the meeting is gone as soon as minutes are tried again.
        match &result {
            Ok(()) => tracing::info!(meeting = %meeting.id, template = %meeting.template, "minutes_written"),
            Err(error) => tracing::warn!(meeting = %meeting.id, %error, "minutes_not_written"),
        }
        meeting.error = result.err().map(|error| format!("Minutes were not written: {error}"));
        if written && settings.email.is_configured() {
            self.send_email(meeting, &settings).await;
        }
        self.update(meeting, Status::Done, 1.0, on_update);
    }

    /// Emails the meeting's minutes again, on request.
    pub async fn email_minutes(&self, id: &str, on_update: &(dyn Fn(&Meeting) + Send + Sync)) {
        let Ok(mut meeting) = self.store.meeting(id) else {
            return;
        };
        self.send_email(&mut meeting, &self.store.settings()).await;
        let _ = self.store.save_meeting(&meeting);
        on_update(&meeting);
    }

    /// A mail that does not go out never fails the meeting: the minutes are safe on disk,
    /// and the reason is kept so it can be shown and the mail sent again.
    async fn send_email(&self, meeting: &mut Meeting, settings: &Settings) {
        let when = chrono::DateTime::parse_from_rfc3339(&meeting.created_at)
            .map(|at| at.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();
        let result = match self.store.minutes(&meeting.id) {
            None => Err("There are no minutes to send yet.".to_string()),
            Some(minutes) => match email::minutes_message(&settings.email, &meeting.title, &when, &minutes) {
                Ok(message) => email::send(&settings.email, message).await,
                Err(error) => Err(error),
            },
        };

        match result {
            Ok(()) => {
                meeting.emailed_to = Some(settings.email.to.trim().to_string());
                meeting.emailed_at = Some(chrono::Local::now().to_rfc3339());
                meeting.email_error = None;
            }
            Err(error) => {
                tracing::warn!(meeting = %meeting.id, %error, "email_failed");
                meeting.email_error = Some(error);
            }
        }
    }

    fn update(
        &self,
        meeting: &mut Meeting,
        status: Status,
        progress: f32,
        on_update: &(dyn Fn(&Meeting) + Send + Sync),
    ) {
        if status != Status::Done {
            meeting.error = None;
        }
        meeting.status = status;
        meeting.progress = progress;
        let _ = self.store.save_meeting(meeting);
        on_update(meeting);
    }

    fn fail(&self, meeting: &mut Meeting, error: String, on_update: &(dyn Fn(&Meeting) + Send + Sync)) {
        tracing::error!(meeting = %meeting.id, %error, "processing_failed");
        meeting.status = Status::Failed;
        meeting.progress = 0.0;
        meeting.error = Some(error);
        let _ = self.store.save_meeting(meeting);
        on_update(meeting);
    }
}

fn llm_config(settings: &Settings) -> LlmConfig {
    LlmConfig {
        base_url: settings.llm_base_url.clone(),
        api_key: settings.llm_api_key.clone(),
        model: settings.llm_model.clone(),
        max_chars_per_call: settings.max_chars_per_call,
    }
}

/// One of the speech models, as the window lists them to choose from.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelChoice {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    /// The whole model, and what of it is not on this machine yet.
    pub size_bytes: u64,
    pub missing_bytes: u64,
    /// Memory it needs while transcribing.
    pub memory_bytes: u64,
    pub chosen: bool,
}

/// Whether transcription can start, and if not, what is missing.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Readiness {
    pub model_found: bool,
    pub model_name: &'static str,
    pub model_location: Option<String>,
    pub model_install_dir: String,
    pub models: Vec<ModelChoice>,
    /// What this machine has, to set next to what a model needs.
    pub total_memory_bytes: u64,
    pub server_found: bool,
    pub server_location: Option<String>,
    /// Without them the transcript simply has no speaker names.
    pub speaker_models_found: bool,
    /// What downloading the missing models would fetch.
    pub download_bytes: u64,
    pub llm_configured: bool,
    pub email_configured: bool,
}

impl Pipeline {
    pub fn readiness(&self) -> Readiness {
        let models_dir = self.store.models_dir();
        let settings = self.store.settings();
        let model = settings.asr_model;
        let files = model.locate_files(&models_dir);
        let server = find_llama_server(&self.bundled_server_dirs);

        Readiness {
            model_found: files.is_some(),
            model_name: model.display_name(),
            model_location: files
                .and_then(|files| files.model.parent().map(|dir| dir.display().to_string())),
            model_install_dir: model.install_dir(&models_dir).display().to_string(),
            models: Qwen3AsrModel::all()
                .iter()
                .map(|choice| ModelChoice {
                    id: choice.as_str(),
                    name: choice.display_name(),
                    description: choice.description(),
                    size_bytes: choice.size_bytes(),
                    missing_bytes: choice.missing_downloads(&models_dir).iter().map(|file| file.size_bytes).sum(),
                    memory_bytes: choice.memory_bytes(),
                    chosen: *choice == model,
                })
                .collect(),
            total_memory_bytes: qwen3_asr::total_memory_bytes(),
            server_found: server.is_some(),
            server_location: server.map(|path| path.display().to_string()),
            speaker_models_found: SpeakerModels::locate(&self.store.speaker_models_dir()).is_some(),
            download_bytes: crate::download::total_bytes(&crate::download::missing_packages(
                &models_dir,
                &self.store.speaker_models_dir(),
                model,
            )),
            llm_configured: !settings.llm_base_url.trim().is_empty()
                && !settings.llm_model.trim().is_empty(),
            email_configured: settings.email.is_configured(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[test]
    fn only_meetings_that_failed_for_want_of_the_speech_model_wait_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().to_path_buf()).unwrap();
        let pipeline = Pipeline {
            store: store.clone(),
            bundled_server_dirs: Vec::new(),
        };
        let at = |hour| chrono::Local::now() - chrono::Duration::hours(hour);
        let mut waiting = store.create_meeting(at(3), "discussion").unwrap();
        waiting.status = Status::Failed;
        waiting.error = Some(format!("{MODEL_MISSING} Download it."));
        let mut broken = store.create_meeting(at(2), "discussion").unwrap();
        broken.status = Status::Failed;
        broken.error = Some("No speech was found in the recording.".to_string());
        let mut done = store.create_meeting(at(1), "discussion").unwrap();
        done.status = Status::Done;
        for meeting in [&waiting, &broken, &done] {
            store.save_meeting(meeting).unwrap();
        }

        assert_eq!(pipeline.waiting_for_model(), [waiting.id]);
    }

    #[test]
    fn an_imported_recording_becomes_a_meeting_named_after_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let pipeline = Pipeline {
            store: store.clone(),
            bundled_server_dirs: Vec::new(),
        };
        let source = dir.path().join("Budget review.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 8_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&source, spec).unwrap();
        (0..16_000).for_each(|i| writer.write_sample((i % 100) as i16).unwrap());
        writer.finalize().unwrap();

        let meeting = pipeline.import(&source).unwrap();

        assert_eq!((meeting.title.as_str(), meeting.status), ("Budget review", Status::Transcribing));
        assert!((meeting.duration_seconds - 2.0).abs() < 0.01);
        assert!(store.audio_path(&meeting.id).is_file() && source.is_file());

        // A file that is no recording leaves no meeting behind.
        let text = dir.path().join("notes.wav");
        std::fs::write(&text, b"not samples").unwrap();
        assert!(pipeline.import(&text).is_err());
        assert_eq!(store.meetings().len(), 1);
    }

    /// Needs `llama-server`, the Qwen3-ASR files and a WAV recording on this machine. With
    /// `ZILLANOTE_SPEAKER_MODELS` pointing at the two speaker models, and a recording of two
    /// or more people, it also checks the speaker names:
    ///
    /// ZILLANOTE_TEST_AUDIO=/path/to.wav cargo test -p engine live_pipeline -- --ignored --nocapture
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs llama-server, the Qwen3-ASR files and ZILLANOTE_TEST_AUDIO"]
    async fn live_pipeline_transcribes_a_recording_and_survives_a_missing_language_model() {
        let audio = std::env::var("ZILLANOTE_TEST_AUDIO").expect("ZILLANOTE_TEST_AUDIO");
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().to_path_buf()).unwrap();
        let mut settings = store.settings();
        settings.llm_base_url = "http://127.0.0.1:9".to_string(); // nothing listens here
        settings.llm_model = "none".to_string();
        store.save_settings(&settings).unwrap();
        let meeting = store.create_meeting(chrono::Local::now(), "discussion").unwrap();
        std::fs::copy(&audio, store.audio_path(&meeting.id)).unwrap();
        let speaker_models = std::env::var("ZILLANOTE_SPEAKER_MODELS").ok();
        if let Some(models) = &speaker_models {
            std::fs::create_dir_all(store.models_dir()).unwrap();
            std::os::unix::fs::symlink(models, store.speaker_models_dir()).unwrap();
        }

        let pipeline = Pipeline {
            store: store.clone(),
            bundled_server_dirs: vec![
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/resources/llama-server"),
            ],
        };
        let statuses = Mutex::new(Vec::new());
        let started = std::time::Instant::now();
        pipeline
            .process(&meeting.id, &|meeting: &Meeting| {
                let mut statuses = statuses.lock().unwrap();
                if statuses.last() != Some(&meeting.status) {
                    statuses.push(meeting.status);
                }
            })
            .await;

        let meeting = store.meeting(&meeting.id).unwrap();
        let transcript = store.transcript(&meeting.id).expect("a transcript");
        println!(
            "{:.0}s of audio in {:?}, {} segments\n{}",
            transcript.duration_seconds,
            started.elapsed(),
            transcript.segments.len(),
            transcript.to_text().lines().take(4).collect::<Vec<_>>().join("\n")
        );

        assert_eq!(
            *statuses.lock().unwrap(),
            [Status::Transcribing, Status::Summarizing, Status::Done]
        );
        assert!(meeting.has_transcript && !meeting.has_minutes);
        assert!(meeting.error.unwrap().contains("Minutes were not written"));
        let last = transcript.segments.last().unwrap();
        assert!(last.end > transcript.duration_seconds * 0.8, "transcript stops at {}", last.end);
        assert!(!transcript.to_text().contains("<asr_text>"));

        if speaker_models.is_none() {
            assert!(transcript.segments.iter().all(|segment| segment.speaker.is_none()));
            return;
        }
        let speakers = store.speakers(&meeting.id);
        println!("{:?}", speakers.iter().map(|s| (&s.label, s.seconds as u32)).collect::<Vec<_>>());
        assert!(speakers.len() >= 2, "{} speaker(s) found", speakers.len());
        assert!(transcript.to_text().contains("Speaker 1: ") && transcript.to_text().contains("Speaker 2: "));

        // A name given once is on this transcript at once, and on the next one by itself.
        pipeline.name_speaker(&meeting.id, 1, "Alex").unwrap();
        let renamed = store.transcript(&meeting.id).unwrap().to_text();
        assert!(renamed.contains("Alex: ") && !renamed.contains("Speaker 2: "));

        let again = store.create_meeting(chrono::Local::now(), "discussion").unwrap();
        std::fs::copy(&audio, store.audio_path(&again.id)).unwrap();
        pipeline.process(&again.id, &|_: &Meeting| {}).await;
        let recognized = store.speakers(&again.id);
        println!("{:?}", recognized.iter().map(|s| (&s.label, s.voice_id.is_some())).collect::<Vec<_>>());
        assert_eq!(recognized.iter().filter(|speaker| speaker.label == "Alex").count(), 1);
        assert_eq!(store.voices()[0].examples.len(), 1, "recognizing a voice must not add to it");
    }
}
