//! Everything the app keeps is plain files: one folder per meeting, one settings file.
//!
//! ```text
//! <data dir>/settings.json
//! <data dir>/models/                      Qwen3-ASR weights, if not taken from LM Studio
//! <data dir>/meetings/<id>/meeting.json   title, status, progress
//!                          audio.wav
//!                          transcript.json
//!                          minutes.md
//! ```

use std::path::{Path, PathBuf};

use crate::email::EmailSettings;
use crate::templates::{DEFAULT_SYSTEM_PROMPT, DEFAULT_TEMPLATE};
use crate::transcript::Transcript;

pub const DATA_DIR_ENV: &str = "ZILLANOTE_DATA_DIR";
const APP_FOLDER: &str = "com.zillanote.lite";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Recording,
    Transcribing,
    Summarizing,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Meeting {
    pub id: String,
    pub title: String,
    pub created_at: String,
    #[serde(default)]
    pub duration_seconds: f64,
    pub status: Status,
    /// 0 to 1 within the current status.
    #[serde(default)]
    pub progress: f32,
    /// Why the meeting failed, or why a finished transcript has no minutes yet.
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default = "default_template")]
    pub template: String,
    #[serde(default)]
    pub has_transcript: bool,
    #[serde(default)]
    pub has_minutes: bool,
    /// Where and when the minutes were last emailed, or why that failed.
    #[serde(default)]
    pub emailed_to: Option<String>,
    #[serde(default)]
    pub emailed_at: Option<String>,
    #[serde(default)]
    pub email_error: Option<String>,
}

fn default_template() -> String {
    DEFAULT_TEMPLATE.to_string()
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Settings {
    pub llm_base_url: String,
    pub llm_api_key: String,
    pub llm_model: String,
    pub system_prompt: String,
    pub default_template: String,
    /// Names and terms the recognizer should prefer.
    pub vocabulary: Vec<String>,
    pub max_chars_per_call: usize,
    pub email: EmailSettings,
    /// Record what the computer plays (the other side of a call) next to the microphone.
    pub record_system_audio: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            // LM Studio's local server.
            llm_base_url: "http://localhost:1234/v1".to_string(),
            llm_api_key: String::new(),
            llm_model: String::new(),
            system_prompt: DEFAULT_SYSTEM_PROMPT.to_string(),
            default_template: DEFAULT_TEMPLATE.to_string(),
            vocabulary: Vec::new(),
            max_chars_per_call: 24_000,
            email: EmailSettings::default(),
            record_system_audio: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn open_default() -> Result<Self, String> {
        let root = match std::env::var_os(DATA_DIR_ENV) {
            Some(dir) => PathBuf::from(dir),
            None => dirs::data_dir()
                .ok_or_else(|| "No application data folder on this system.".to_string())?
                .join(APP_FOLDER),
        };
        Self::open(root)
    }

    pub fn open(root: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(root.join("meetings")).map_err(|e| e.to_string())?;
        Ok(Self { root })
    }

    pub fn models_dir(&self) -> PathBuf {
        self.root.join("models")
    }

    pub fn meeting_dir(&self, id: &str) -> PathBuf {
        self.root.join("meetings").join(id)
    }

    pub fn audio_path(&self, id: &str) -> PathBuf {
        self.meeting_dir(id).join("audio.wav")
    }

    // --- settings ---

    pub fn settings(&self) -> Settings {
        std::fs::read_to_string(self.root.join("settings.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<(), String> {
        let path = self.root.join("settings.json");
        write_json(&path, settings)?;
        // It can hold an API key.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    // --- meetings ---

    pub fn create_meeting(&self, now: chrono::DateTime<chrono::Local>, template: &str) -> Result<Meeting, String> {
        let base = now.format("%Y-%m-%d-%H%M%S").to_string();
        let id = (0..)
            .map(|n| if n == 0 { base.clone() } else { format!("{base}-{n}") })
            .find(|id| !self.meeting_dir(id).exists())
            .expect("an unused id");
        std::fs::create_dir_all(self.meeting_dir(&id)).map_err(|e| e.to_string())?;

        let meeting = Meeting {
            title: format!("Meeting {}", now.format("%Y-%m-%d %H:%M")),
            created_at: now.to_rfc3339(),
            id,
            duration_seconds: 0.0,
            status: Status::Recording,
            progress: 0.0,
            error: None,
            template: template.to_string(),
            has_transcript: false,
            has_minutes: false,
            emailed_to: None,
            emailed_at: None,
            email_error: None,
        };
        self.save_meeting(&meeting)?;
        Ok(meeting)
    }

    pub fn save_meeting(&self, meeting: &Meeting) -> Result<(), String> {
        write_json(&self.meeting_dir(&meeting.id).join("meeting.json"), meeting)
    }

    pub fn meeting(&self, id: &str) -> Result<Meeting, String> {
        let path = self.meeting_dir(id).join("meeting.json");
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Newest first.
    pub fn meetings(&self) -> Vec<Meeting> {
        let mut meetings = std::fs::read_dir(self.root.join("meetings"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| self.meeting(&entry.file_name().to_string_lossy()).ok())
            .collect::<Vec<_>>();
        meetings.sort_by(|a, b| b.id.cmp(&a.id));
        meetings
    }

    pub fn delete_meeting(&self, id: &str) -> Result<(), String> {
        // Ids are folder names the app made; refuse anything that could point elsewhere.
        if id.is_empty() || id.contains(['/', '\\']) || id.contains("..") {
            return Err("Not a meeting id.".to_string());
        }
        std::fs::remove_dir_all(self.meeting_dir(id)).map_err(|e| e.to_string())
    }

    /// Work that was under way when the app last closed cannot still be running.
    pub fn mark_interrupted(&self) {
        for mut meeting in self.meetings() {
            if matches!(
                meeting.status,
                Status::Recording | Status::Transcribing | Status::Summarizing
            ) {
                meeting.status = Status::Failed;
                meeting.progress = 0.0;
                meeting.error = Some("The app closed before this finished. Process it again.".to_string());
                let _ = self.save_meeting(&meeting);
            }
        }
    }

    pub fn save_transcript(&self, id: &str, transcript: &Transcript) -> Result<(), String> {
        write_json(&self.meeting_dir(id).join("transcript.json"), transcript)
    }

    pub fn transcript(&self, id: &str) -> Option<Transcript> {
        let text = std::fs::read_to_string(self.meeting_dir(id).join("transcript.json")).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn save_minutes(&self, id: &str, minutes: &str) -> Result<(), String> {
        write_atomically(&self.meeting_dir(id).join("minutes.md"), minutes.as_bytes())
    }

    pub fn minutes(&self, id: &str) -> Option<String> {
        std::fs::read_to_string(self.meeting_dir(id).join("minutes.md")).ok()
    }
}

fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    write_atomically(path, text.as_bytes())
}

/// Through a temporary file, so a crash mid-write never leaves half a file behind.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, bytes).map_err(|e| format!("{}: {e}", temporary.display()))?;
    std::fs::rename(&temporary, path).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().to_path_buf()).unwrap();
        (dir, store)
    }

    fn at(hour: u32) -> chrono::DateTime<chrono::Local> {
        chrono::Local.with_ymd_and_hms(2026, 9, 20, hour, 30, 0).unwrap()
    }

    #[test]
    fn meetings_are_listed_newest_first_and_ids_never_collide() {
        let (_dir, store) = store();
        let first = store.create_meeting(at(9), "discussion").unwrap();
        let same_second = store.create_meeting(at(9), "discussion").unwrap();
        let later = store.create_meeting(at(14), "business").unwrap();

        assert_ne!(first.id, same_second.id);
        assert_eq!(first.title, "Meeting 2026-09-20 09:30");
        assert_eq!(
            store.meetings().iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            [later.id.as_str(), same_second.id.as_str(), first.id.as_str()]
        );
    }

    #[test]
    fn settings_start_from_the_defaults_and_survive_a_round_trip() {
        let (_dir, store) = store();
        assert_eq!(store.settings(), Settings::default());

        let mut settings = store.settings();
        settings.system_prompt = "Be brief.".to_string();
        settings.llm_model = "qwen/qwen3.8-27b".to_string();
        store.save_settings(&settings).unwrap();

        assert_eq!(store.settings(), settings);
    }

    #[test]
    fn settings_written_by_an_older_version_keep_loading() {
        let (dir, store) = store();
        std::fs::write(dir.path().join("settings.json"), r#"{"llm_model":"m"}"#).unwrap();

        let settings = store.settings();

        assert_eq!(settings.llm_model, "m");
        assert_eq!(settings.system_prompt, DEFAULT_SYSTEM_PROMPT);
        assert!(settings.record_system_audio);
    }

    #[test]
    fn unfinished_work_is_marked_failed_on_the_next_start() {
        let (_dir, store) = store();
        let mut done = store.create_meeting(at(9), "discussion").unwrap();
        done.status = Status::Done;
        store.save_meeting(&done).unwrap();
        let recording = store.create_meeting(at(10), "discussion").unwrap();

        store.mark_interrupted();

        assert_eq!(store.meeting(&done.id).unwrap().status, Status::Done);
        let interrupted = store.meeting(&recording.id).unwrap();
        assert_eq!(interrupted.status, Status::Failed);
        assert!(interrupted.error.unwrap().contains("Process it again"));
    }

    #[test]
    fn deleting_only_accepts_meeting_ids() {
        let (dir, store) = store();
        let meeting = store.create_meeting(at(9), "discussion").unwrap();

        assert!(store.delete_meeting("../meetings").is_err());
        assert!(store.delete_meeting("").is_err());
        store.delete_meeting(&meeting.id).unwrap();

        assert!(store.meetings().is_empty());
        assert!(dir.path().join("meetings").exists());
    }
}
