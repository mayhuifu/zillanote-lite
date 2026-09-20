//! What happens after Stop: transcribe, then write minutes, one model in memory at a time.

use std::path::PathBuf;

use qwen3_asr::{
    LlamaServer, LlamaServerConfig, Qwen3AsrClient, Qwen3AsrModel, describe_missing_binary,
    describe_missing_model, find_llama_server, kill_stale_servers,
};

use crate::audio::{TARGET_RATE, read_wav_16k_channels};
use crate::chunker::{ChunkerConfig, speech_chunks};
use crate::email;
use crate::minutes::{LlmConfig, write_minutes};
use crate::mixdown::recognition_signal;
use crate::store::{Meeting, Settings, Status, Store};
use crate::templates::template;
use crate::transcript::{Segment, Transcript};

pub const MODEL: Qwen3AsrModel = Qwen3AsrModel::Large;

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
        self.summarize(&mut meeting, on_update).await;
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

        let audio_path = self.store.audio_path(&meeting.id);
        let samples = tokio::task::spawn_blocking(move || read_wav_16k_channels(&audio_path).map(recognition_signal))
            .await
            .map_err(|e| e.to_string())??;
        let duration_seconds = samples.len() as f64 / TARGET_RATE as f64;
        let chunks = speech_chunks(&samples, TARGET_RATE, ChunkerConfig::default());
        if chunks.is_empty() {
            return Err("No speech was found in the recording.".to_string());
        }

        let settings = self.store.settings();
        let mut server = self.start_server().await?;
        let client = Qwen3AsrClient::new(&server.base_url(), MODEL)
            .map_err(|e| e.to_string())?
            .with_vocabulary(&settings.vocabulary);

        let mut segments = Vec::new();
        let mut failures = 0;
        let mut last_error = String::new();
        for (index, chunk) in chunks.iter().enumerate() {
            match client.transcribe_samples(&samples[chunk.start..chunk.end]).await {
                Ok(text) if !text.trim().is_empty() => segments.push(Segment {
                    start: chunk.start as f64 / TARGET_RATE as f64,
                    end: chunk.end as f64 / TARGET_RATE as f64,
                    text: text.trim().to_string(),
                    speaker: None,
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
                (index + 1) as f32 / chunks.len() as f32,
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
            engine: MODEL.as_str().to_string(),
            segments,
        };
        if transcript.is_empty() {
            return Err("No speech was recognized in the recording.".to_string());
        }
        self.store.save_transcript(&meeting.id, &transcript)?;
        meeting.duration_seconds = duration_seconds;
        meeting.has_transcript = true;
        Ok(())
    }

    async fn start_server(&self) -> Result<LlamaServer, String> {
        let models_dir = self.store.models_dir();
        let files = MODEL
            .locate_files(&models_dir)
            .ok_or_else(|| describe_missing_model(&MODEL.install_dir(&models_dir), MODEL.repo()))?;
        let binary = find_llama_server(&self.bundled_server_dirs).ok_or_else(describe_missing_binary)?;

        // A run of the app that crashed leaves its server behind, holding the model in memory.
        let _ = tokio::task::spawn_blocking(kill_stale_servers).await;
        LlamaServer::start(LlamaServerConfig::new(binary, files))
            .await
            .map_err(|e| e.to_string())
    }

    async fn summarize(&self, meeting: &mut Meeting, on_update: &(dyn Fn(&Meeting) + Send + Sync)) {
        let Some(transcript) = self.store.transcript(&meeting.id) else {
            self.fail(meeting, "There is no transcript to write minutes from.".to_string(), on_update);
            return;
        };
        self.update(meeting, Status::Summarizing, 0.0, on_update);

        let settings = self.store.settings();
        let result = write_minutes(
            &llm_config(&settings),
            &settings.system_prompt,
            template(&meeting.template),
            &meeting.title,
            &transcript.to_text(),
            |_, _| {},
        )
        .await
        .and_then(|minutes| self.store.save_minutes(&meeting.id, &minutes));

        // The transcript is the hard part and it is safe: a meeting without minutes is
        // still done, with the reason shown and "write minutes" one click away.
        meeting.has_minutes = self.store.minutes(&meeting.id).is_some();
        let written = result.is_ok();
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

/// Whether transcription can start, and if not, what is missing.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Readiness {
    pub model_found: bool,
    pub model_location: Option<String>,
    pub model_install_dir: String,
    pub server_found: bool,
    pub server_location: Option<String>,
    pub llm_configured: bool,
    pub email_configured: bool,
}

impl Pipeline {
    pub fn readiness(&self) -> Readiness {
        let models_dir = self.store.models_dir();
        let files = MODEL.locate_files(&models_dir);
        let server = find_llama_server(&self.bundled_server_dirs);
        let settings = self.store.settings();

        Readiness {
            model_found: files.is_some(),
            model_location: files
                .and_then(|files| files.model.parent().map(|dir| dir.display().to_string())),
            model_install_dir: MODEL.install_dir(&models_dir).display().to_string(),
            server_found: server.is_some(),
            server_location: server.map(|path| path.display().to_string()),
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

    /// Needs `llama-server`, the Qwen3-ASR files and a WAV recording on this machine:
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
    }
}
