//! Everything the app keeps is plain files: one folder per meeting, one settings file.
//!
//! ```text
//! <data dir>/settings.json
//! <data dir>/voices.json                  the voices the user has named
//! <data dir>/models/                      Qwen3-ASR weights, if not taken from LM Studio
//! <data dir>/models/speakers/             the two speaker models
//! <data dir>/meetings/<id>/meeting.json   title, status, progress
//!                          audio.wav
//!                          transcript.json
//!                          speakers.json  who was heard, and what they sound like
//!                          minutes.md
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::email::EmailSettings;
use crate::secrets::{self, SecretStore};
use crate::templates::{DEFAULT_SYSTEM_PROMPT, DEFAULT_TEMPLATE};
use crate::transcript::Transcript;
use crate::voices::{MeetingSpeaker, Voice};

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
    /// Where the API key and the mail password go instead of `settings.json`, if anywhere.
    secrets: Option<Arc<dyn SecretStore>>,
}

impl Store {
    /// The user's own data folder, with secrets in the system keychain where this build
    /// uses it. A folder named by `ZILLANOTE_DATA_DIR` is somebody's experiment: it keeps
    /// to itself, secrets included.
    pub fn open_default() -> Result<Self, String> {
        match std::env::var_os(DATA_DIR_ENV) {
            Some(dir) => Self::open(PathBuf::from(dir)),
            None => {
                let root = dirs::data_dir()
                    .ok_or_else(|| "No application data folder on this system.".to_string())?
                    .join(APP_FOLDER);
                Ok(Self::open(root)?.with_secrets(secrets::system()))
            }
        }
    }

    pub fn open(root: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(root.join("meetings")).map_err(|e| e.to_string())?;
        Ok(Self { root, secrets: None })
    }

    pub fn with_secrets(mut self, secrets: Option<Arc<dyn SecretStore>>) -> Self {
        self.secrets = secrets;
        self
    }

    pub fn models_dir(&self) -> PathBuf {
        self.root.join("models")
    }

    pub fn speaker_models_dir(&self) -> PathBuf {
        self.models_dir().join("speakers")
    }

    pub fn meeting_dir(&self, id: &str) -> PathBuf {
        self.root.join("meetings").join(id)
    }

    pub fn audio_path(&self, id: &str) -> PathBuf {
        self.meeting_dir(id).join("audio.wav")
    }

    // --- settings ---

    pub fn settings(&self) -> Settings {
        let mut settings = self.settings_on_disk();
        // A secret still in the file (an older version wrote it, or the keychain refused it)
        // counts; otherwise it is wherever secrets are kept.
        if let Some(secrets) = &self.secrets {
            for (name, value) in [
                (secrets::LLM_API_KEY, &mut settings.llm_api_key),
                (secrets::EMAIL_PASSWORD, &mut settings.email.password),
            ] {
                if value.is_empty() {
                    match secrets.get(name) {
                        Ok(secret) => *value = secret.unwrap_or_default(),
                        Err(error) => tracing::warn!(name, %error, "secret_unreadable"),
                    }
                }
            }
        }
        settings
    }

    /// What `settings.json` itself holds, secrets kept elsewhere left out.
    fn settings_on_disk(&self) -> Settings {
        std::fs::read_to_string(self.root.join("settings.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<(), String> {
        let path = self.root.join("settings.json");
        let mut on_disk = settings.clone();
        if let Some(secrets) = &self.secrets {
            for (name, value) in [
                (secrets::LLM_API_KEY, &mut on_disk.llm_api_key),
                (secrets::EMAIL_PASSWORD, &mut on_disk.email.password),
            ] {
                // An empty field only means "remove it" if the secret could have been shown.
                // When the keychain cannot be read the field is empty for that reason alone,
                // and what is stored stays as it is.
                if value.is_empty() && secrets.get(name).is_err() {
                    continue;
                }
                // A secret the keychain will not take stays in the file: never lost.
                match secrets.set(name, value) {
                    Ok(()) => value.clear(),
                    Err(error) => tracing::warn!(name, %error, "secret_kept_in_settings_file"),
                }
            }
        }
        write_json(&path, &on_disk)?;
        // It can hold an API key.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    /// Moves secrets an older version left in `settings.json` to where they are kept now.
    /// With none in the file there is nothing to do, and nothing is touched.
    pub fn migrate_secrets(&self) {
        let on_disk = self.settings_on_disk();
        let in_file = !on_disk.llm_api_key.is_empty() || !on_disk.email.password.is_empty();
        if self.secrets.is_some() && in_file {
            let _ = self.save_settings(&self.settings());
        }
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

    pub fn save_speakers(&self, id: &str, speakers: &[MeetingSpeaker]) -> Result<(), String> {
        write_json(&self.meeting_dir(id).join("speakers.json"), &speakers)
    }

    pub fn speakers(&self, id: &str) -> Vec<MeetingSpeaker> {
        std::fs::read_to_string(self.meeting_dir(id).join("speakers.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save_voices(&self, voices: &[Voice]) -> Result<(), String> {
        write_json(&self.root.join("voices.json"), &voices)
    }

    pub fn voices(&self) -> Vec<Voice> {
        std::fs::read_to_string(self.root.join("voices.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
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

    fn secret_settings() -> Settings {
        let mut settings = Settings::default();
        settings.llm_api_key = "sk-secret".to_string();
        settings.email.password = "app-password".to_string();
        settings
    }

    #[test]
    fn secrets_go_to_the_keychain_and_not_into_the_file() {
        let (dir, store) = store();
        let keychain = Arc::new(secrets::MemorySecrets::default());
        let store = store.with_secrets(Some(keychain.clone()));

        store.save_settings(&secret_settings()).unwrap();

        let file = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();
        assert!(!file.contains("sk-secret") && !file.contains("app-password"), "{file}");
        assert_eq!(keychain.values.lock().unwrap().len(), 2);
        assert_eq!(store.settings(), secret_settings());

        // Clearing a secret in Settings clears it for good.
        store.save_settings(&Settings::default()).unwrap();
        assert!(keychain.values.lock().unwrap().is_empty());
        assert_eq!(store.settings(), Settings::default());
    }

    #[test]
    fn secrets_an_older_version_left_in_the_file_are_moved_on_start() {
        let (dir, store) = store();
        store.save_settings(&secret_settings()).unwrap();
        let store = store.with_secrets(Some(Arc::new(secrets::MemorySecrets::default())));
        assert_eq!(store.settings(), secret_settings());

        store.migrate_secrets();

        assert!(!std::fs::read_to_string(dir.path().join("settings.json")).unwrap().contains("sk-secret"));
        assert_eq!(store.settings(), secret_settings());
    }

    #[test]
    fn a_keychain_that_cannot_be_read_keeps_what_it_holds() {
        // The user said no to the keychain's question: the fields come back empty, the app
        // starts, Settings is saved. None of that may remove what is stored.
        let (_dir, store) = store();
        let denied = Arc::new(secrets::MemorySecrets {
            unreadable: true,
            ..Default::default()
        });
        denied.values.lock().unwrap().insert(secrets::LLM_API_KEY.to_string(), "sk-secret".to_string());
        let store = store.with_secrets(Some(denied.clone()));

        assert_eq!(store.settings().llm_api_key, "");
        store.migrate_secrets();
        store.save_settings(&store.settings()).unwrap();

        assert_eq!(denied.values.lock().unwrap().get(secrets::LLM_API_KEY).unwrap(), "sk-secret");
    }

    #[test]
    fn a_keychain_that_refuses_never_costs_the_secret() {
        let (dir, store) = store();
        let broken = secrets::MemorySecrets {
            broken: true,
            ..Default::default()
        };
        let store = store.with_secrets(Some(Arc::new(broken)));

        store.save_settings(&secret_settings()).unwrap();

        assert!(std::fs::read_to_string(dir.path().join("settings.json")).unwrap().contains("sk-secret"));
        assert_eq!(store.settings(), secret_settings());
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
