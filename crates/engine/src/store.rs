//! Everything the app keeps is plain files: one folder per meeting, one settings file.
//!
//! ```text
//! <data dir>/settings.json
//! <data dir>/chatgpt.json                the ChatGPT sign-in, its tokens kept with the other secrets
//! <data dir>/zillanote.log                what the app did, for when something went wrong
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

use crate::chatgpt::{ChatGpt, Tokens};
use crate::email::EmailSettings;
use crate::secrets::{self, SecretStore};
use crate::templates::{DEFAULT_SYSTEM_PROMPT, DEFAULT_TEMPLATE, PREVIOUS_SYSTEM_PROMPTS};
use crate::transcript::Transcript;
use crate::voices::{MeetingSpeaker, Voice};

pub const DATA_DIR_ENV: &str = "ZILLANOTE_DATA_DIR";
const APP_FOLDER: &str = "com.zillanote.lite";

/// Who writes the minutes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmProvider {
    /// Any endpoint with OpenAI's chat completions API, named in Settings: LM Studio or
    /// Ollama on this computer, or a hosted API with a key.
    #[default]
    Compatible,
    /// The user's ChatGPT plan, after signing in with ChatGPT (`chatgpt.rs`).
    Chatgpt,
}

/// Who turns the speech into text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AsrProvider {
    /// Qwen3-ASR on this computer: the audio stays here.
    #[default]
    Local,
    /// A service with OpenAI's transcription API, named in Settings: each stretch of speech
    /// is sent to it.
    Service,
}

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
    pub llm_provider: LlmProvider,
    /// The model to ask for on the ChatGPT plan, by its slug.
    pub chatgpt_model: String,
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
    /// Write the minutes as soon as the transcript is ready, without being asked.
    pub auto_minutes: bool,
    /// Make the record button flash when a call program opens the microphone.
    pub notice_calls: bool,
    /// Stop the recording half a minute after the call program lets the microphone go.
    pub stop_when_call_ends: bool,
    /// Which of the speech models transcribes, when that happens on this computer; see
    /// [`Settings::speech_model`] for one the user has not picked.
    pub asr_model: qwen3_asr::Qwen3AsrModel,
    /// The user picked `asr_model` in Settings. Until then it is only where the app started,
    /// and a model already on this computer is used instead of downloading it.
    pub asr_model_picked: bool,
    pub asr_provider: AsrProvider,
    /// The speech service, when that is the choice: its OpenAI-style root, for example
    /// `https://api.openai.com/v1`, the model to ask for, and the key.
    pub asr_service_url: String,
    pub asr_service_model: String,
    pub asr_service_key: String,
}

impl Settings {
    /// The speech model that transcribes on this computer. One the user picked is that one,
    /// here or not: picking a model is asking for it. Otherwise the one in the settings if it
    /// is here, else one that is (left from an earlier version, or in LM Studio's folder), and
    /// only when none is, the one in the settings, to download. Nobody is asked to download
    /// the default when a model they have works.
    pub fn speech_model(&self, models_dir: &Path) -> qwen3_asr::Qwen3AsrModel {
        self.speech_model_among(|model| model.locate_files(models_dir).is_some())
    }

    fn speech_model_among(&self, is_here: impl Fn(qwen3_asr::Qwen3AsrModel) -> bool) -> qwen3_asr::Qwen3AsrModel {
        if self.asr_model_picked || is_here(self.asr_model) {
            return self.asr_model;
        }
        qwen3_asr::Qwen3AsrModel::all().iter().copied().find(|model| is_here(*model)).unwrap_or(self.asr_model)
    }

    /// Whether a speech service is named well enough to be asked.
    pub fn asr_service_configured(&self) -> bool {
        !self.asr_service_url.trim().is_empty() && !self.asr_service_model.trim().is_empty()
    }

    /// The fields that are secrets, by the names they are kept under.
    fn secrets_mut(&mut self) -> [(&'static str, &mut String); 3] {
        [
            (secrets::LLM_API_KEY, &mut self.llm_api_key),
            (secrets::EMAIL_PASSWORD, &mut self.email.password),
            (secrets::ASR_SERVICE_KEY, &mut self.asr_service_key),
        ]
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            llm_provider: LlmProvider::Compatible,
            chatgpt_model: String::new(),
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
            auto_minutes: true,
            notice_calls: true,
            stop_when_call_ends: true,
            asr_model: qwen3_asr::Qwen3AsrModel::default(),
            asr_model_picked: false,
            asr_provider: AsrProvider::Local,
            asr_service_url: String::new(),
            asr_service_model: String::new(),
            asr_service_key: String::new(),
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

    pub fn log_path(&self) -> PathBuf {
        self.root.join("zillanote.log")
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
        // A prompt that is still word for word what an earlier version started with was
        // never edited: it moves on with the app. One the user has touched is theirs.
        if PREVIOUS_SYSTEM_PROMPTS.iter().any(|previous| previous.trim() == settings.system_prompt.trim()) {
            settings.system_prompt = DEFAULT_SYSTEM_PROMPT.to_string();
        }
        // A secret still in the file (an older version wrote it, or the keychain refused it)
        // counts; otherwise it is wherever secrets are kept.
        if let Some(secrets) = &self.secrets {
            for (name, value) in settings.secrets_mut() {
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
    /// The settings without their secrets, which never asks the keychain: for something that
    /// looks at a switch every second.
    pub fn settings_without_secrets(&self) -> Settings {
        let mut settings = self.settings_on_disk();
        for (_, value) in settings.secrets_mut() {
            value.clear();
        }
        settings
    }

    fn settings_on_disk(&self) -> Settings {
        read_json_or_default(&self.root.join("settings.json"))
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<(), String> {
        let path = self.root.join("settings.json");
        let mut on_disk = settings.clone();
        if let Some(secrets) = &self.secrets {
            for (name, value) in on_disk.secrets_mut() {
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
        let mut on_disk = self.settings_on_disk();
        let in_file = on_disk.secrets_mut().iter().any(|(_, value)| !value.is_empty());
        if self.secrets.is_some() && in_file {
            let _ = self.save_settings(&self.settings());
        }
    }

    // --- ChatGPT ---

    /// The ChatGPT sign-in, with its tokens: these come from the keychain where this build
    /// keeps secrets, so this may ask the keychain.
    pub fn chatgpt(&self) -> ChatGpt {
        let mut saved = self.chatgpt_file();
        if saved.tokens.is_none()
            && let Some(secrets) = &self.secrets
        {
            match secrets.get(secrets::CHATGPT_TOKENS) {
                Ok(text) => saved.tokens = text.and_then(|text| serde_json::from_str::<Tokens>(&text).ok()),
                Err(error) => tracing::warn!(%error, "chatgpt_tokens_unreadable"),
            }
        }
        saved
    }

    /// The ChatGPT sign-in without its tokens, which never asks the keychain.
    pub fn chatgpt_without_tokens(&self) -> ChatGpt {
        ChatGpt {
            tokens: None,
            ..self.chatgpt_file()
        }
    }

    /// What `chatgpt.json` itself holds: tokens only where no keychain took them.
    fn chatgpt_file(&self) -> ChatGpt {
        read_json_or_default(&self.root.join("chatgpt.json"))
    }

    /// No tokens means none: they are removed from wherever they were kept. Tokens the keychain
    /// will not take stay in the file, as other secrets do.
    pub fn save_chatgpt(&self, value: &ChatGpt) -> Result<(), String> {
        let mut on_disk = value.clone();
        if let Some(secrets) = &self.secrets {
            let text = match &on_disk.tokens {
                Some(tokens) => serde_json::to_string(tokens).map_err(|e| e.to_string())?,
                None => String::new(),
            };
            match secrets.set(secrets::CHATGPT_TOKENS, &text) {
                Ok(()) => on_disk.tokens = None,
                Err(error) => tracing::warn!(%error, "chatgpt_tokens_kept_in_file"),
            }
        }
        let path = self.root.join("chatgpt.json");
        write_json(&path, &on_disk)?;
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

    /// Work that was under way when the app last closed cannot still be running. A meeting
    /// that was only waiting for its minutes keeps its transcript and is returned, so the
    /// minutes can be written now; anything earlier has to be processed again.
    pub fn mark_interrupted(&self) -> Vec<String> {
        let mut owed_minutes = Vec::new();
        for mut meeting in self.meetings() {
            match meeting.status {
                Status::Summarizing if meeting.has_transcript => {
                    meeting.status = Status::Done;
                    meeting.error = Some("Minutes were not written: ZillaNote was closed first.".to_string());
                    owed_minutes.push(meeting.id.clone());
                }
                Status::Recording | Status::Transcribing | Status::Summarizing => {
                    meeting.status = Status::Failed;
                    meeting.error = Some("The app closed before this finished. Process it again.".to_string());
                }
                Status::Done | Status::Failed => continue,
            }
            meeting.progress = 0.0;
            let _ = self.save_meeting(&meeting);
        }
        owed_minutes
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
        read_json_or_default(&self.root.join("voices.json"))
    }

    pub fn save_minutes(&self, id: &str, minutes: &str) -> Result<(), String> {
        write_atomically(&self.meeting_dir(id).join("minutes.md"), minutes.as_bytes())
    }

    pub fn minutes(&self, id: &str) -> Option<String> {
        std::fs::read_to_string(self.meeting_dir(id).join("minutes.md")).ok()
    }
}

/// The user's settings, voices and sign-in: the defaults when there is no file yet. A file that
/// is there but cannot be read (damaged, or written by a newer version in a shape this one
/// does not know) is moved aside, never written over: the next save would otherwise replace
/// what the user set up with the defaults for good. It can be put back by hand.
fn read_json_or_default<T: serde::de::DeserializeOwned + Default>(path: &Path) -> T {
    let Ok(text) = std::fs::read_to_string(path) else {
        return T::default();
    };
    match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(error) => {
            let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
            let aside = path.with_extension(format!("unreadable-{stamp}.json"));
            tracing::error!(path = %path.display(), %error, aside = %aside.display(), "file_unreadable_moved_aside");
            let _ = std::fs::rename(path, &aside);
            T::default()
        }
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
        assert_eq!(settings.system_prompt, *DEFAULT_SYSTEM_PROMPT);
        assert!(settings.record_system_audio);
        assert!(settings.auto_minutes);
        // Transcription stays on this computer unless the user names a service.
        assert_eq!(settings.asr_provider, AsrProvider::Local);
        assert!(!settings.asr_service_configured());
    }

    fn secret_settings() -> Settings {
        let mut settings = Settings::default();
        settings.llm_api_key = "sk-secret".to_string();
        settings.email.password = "app-password".to_string();
        settings.asr_service_key = "asr-secret".to_string();
        settings
    }

    #[test]
    fn secrets_go_to_the_keychain_and_not_into_the_file() {
        let (dir, store) = store();
        let keychain = Arc::new(secrets::MemorySecrets::default());
        let store = store.with_secrets(Some(keychain.clone()));

        store.save_settings(&secret_settings()).unwrap();

        let file = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();
        assert!(!file.contains("sk-secret") && !file.contains("app-password") && !file.contains("asr-secret"), "{file}");
        assert_eq!(keychain.values.lock().unwrap().len(), 3);
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

        let file = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();
        assert!(!file.contains("sk-secret") && !file.contains("asr-secret"), "{file}");
        assert_eq!(store.settings(), secret_settings());
    }

    /// `settings.json` as 0.3.1 writes it, with a prompt of the user's own: everything in it
    /// must come through an upgrade as it was, and a save must keep it.
    const SETTINGS_0_3_1: &str = r#"{
  "llm_base_url": "https://api.example.com/v1",
  "llm_api_key": "",
  "llm_model": "example-model",
  "system_prompt": "My own rules.\n\nTemplate: decisions first.",
  "default_template": "business",
  "vocabulary": ["U300", "RedCap", "Zillanote"],
  "max_chars_per_call": 30000,
  "email": { "to": "me@example.com", "from": "me@qq.com", "password": "", "server": "" },
  "record_system_audio": false,
  "auto_minutes": true,
  "notice_calls": false,
  "stop_when_call_ends": true,
  "asr_model": "qwen3-asr-1.7b",
  "asr_provider": "local",
  "asr_service_url": "",
  "asr_service_model": "",
  "asr_service_key": ""
}"#;

    #[test]
    fn settings_of_the_previous_version_come_through_an_upgrade_and_a_save_unchanged() {
        let (dir, store) = store();
        std::fs::write(dir.path().join("settings.json"), SETTINGS_0_3_1).unwrap();

        let settings = store.settings();

        assert_eq!(settings.llm_base_url, "https://api.example.com/v1");
        assert_eq!(settings.llm_model, "example-model");
        assert_eq!(settings.system_prompt, "My own rules.\n\nTemplate: decisions first.");
        assert_eq!(settings.default_template, "business");
        assert_eq!(settings.vocabulary, ["U300", "RedCap", "Zillanote"]);
        assert_eq!(settings.max_chars_per_call, 30000);
        assert_eq!((settings.email.to.as_str(), settings.email.from.as_str()), ("me@example.com", "me@qq.com"));
        assert!(!settings.record_system_audio && !settings.notice_calls);
        assert_eq!(settings.asr_model, qwen3_asr::Qwen3AsrModel::Large);
        // What is new starts where it changes nothing: the user's own model writes the minutes.
        assert_eq!(settings.llm_provider, LlmProvider::Compatible);
        assert!(settings.chatgpt_model.is_empty() && !settings.asr_model_picked);

        // Saved again (any change in Settings does that), every old value is still there.
        store.save_settings(&settings).unwrap();
        let old: serde_json::Value = serde_json::from_str(SETTINGS_0_3_1).unwrap();
        let new: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.path().join("settings.json")).unwrap()).unwrap();
        for (key, value) in old.as_object().unwrap() {
            assert_eq!(&new[key], value, "{key}");
        }
    }

    #[test]
    fn a_settings_file_this_version_cannot_read_is_set_aside_not_written_over() {
        let (dir, store) = store();
        std::fs::write(dir.path().join("settings.json"), r#"{"vocabulary": "not a list"}"#).unwrap();

        let settings = store.settings();
        store.save_settings(&settings).unwrap();

        let aside: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.starts_with("settings.unreadable-"))
            .collect();
        assert_eq!(aside.len(), 1, "{aside:?}");
        assert_eq!(std::fs::read_to_string(dir.path().join(&aside[0])).unwrap(), r#"{"vocabulary": "not a list"}"#);
    }

    /// Reads a real data folder's settings and voices the way this version does, from copies,
    /// and checks that nothing in them is lost, including after a save. The folder is only read:
    ///
    /// ZILLANOTE_UPGRADE_FROM=~/Library/Application\ Support/com.zillanote.lite \
    ///   cargo test -p engine live_upgrade -- --ignored --nocapture
    #[test]
    #[ignore = "needs ZILLANOTE_UPGRADE_FROM, a data folder of an earlier version"]
    fn live_upgrade_keeps_the_settings_and_voices_of_a_real_folder() {
        let from = PathBuf::from(std::env::var("ZILLANOTE_UPGRADE_FROM").expect("ZILLANOTE_UPGRADE_FROM"));
        let (dir, store) = store();
        for name in ["settings.json", "voices.json"] {
            if from.join(name).exists() {
                std::fs::copy(from.join(name), dir.path().join(name)).unwrap();
            }
        }
        let old_text = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();
        let old: serde_json::Value = serde_json::from_str(&old_text).unwrap();
        let old_voices: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("voices.json")).unwrap_or("[]".into())).unwrap();

        let settings = store.settings();
        let voices = store.voices();
        store.save_settings(&settings).unwrap();
        store.save_voices(&voices).unwrap();

        let new: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("settings.json")).unwrap()).unwrap();
        let prompt_moved_on = PREVIOUS_SYSTEM_PROMPTS.iter().any(|previous| previous.trim() == old["system_prompt"].as_str().unwrap_or("").trim());
        for (key, value) in old.as_object().unwrap() {
            if key == "system_prompt" && prompt_moved_on {
                continue; // an untouched earlier default moves on to the new one, by design
            }
            assert_eq!(&new[key], value, "{key} changed");
        }
        let new_voices: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("voices.json")).unwrap()).unwrap();
        assert_eq!(new_voices, old_voices, "the voices changed");
        assert!(!std::fs::read_dir(dir.path()).unwrap().any(|entry| {
            entry.unwrap().file_name().to_string_lossy().contains("unreadable")
        }));
        let prompt = if prompt_moved_on {
            "an earlier default, moved on to the new one"
        } else if settings.system_prompt.trim() == DEFAULT_SYSTEM_PROMPT.trim() {
            "the default"
        } else {
            "the user's own, kept"
        };
        println!(
            "{} settings kept; system prompt: {prompt}; {} terms; {} voices; email to set: {}; minutes by: {:?}",
            old.as_object().unwrap().len(),
            settings.vocabulary.len(),
            voices.len(),
            !settings.email.to.is_empty(),
            settings.llm_provider,
        );
    }

    #[test]
    fn a_speech_model_already_here_is_used_rather_than_downloading_the_default() {
        use qwen3_asr::Qwen3AsrModel::{Large, SmallQ4};
        let settings = Settings::default();
        assert_eq!(settings.asr_model, SmallQ4);

        // Only the 1.7B is here (an earlier version's download, or LM Studio's).
        assert_eq!(settings.speech_model_among(|model| model == Large), Large);
        // Both, or only the default: the default. Neither: the default, to download.
        assert_eq!(settings.speech_model_among(|_| true), SmallQ4);
        assert_eq!(settings.speech_model_among(|model| model == SmallQ4), SmallQ4);
        assert_eq!(settings.speech_model_among(|_| false), SmallQ4);
    }

    #[test]
    fn a_speech_model_the_user_picked_is_kept_even_when_another_is_here() {
        use qwen3_asr::Qwen3AsrModel::{Large, SmallQ4};
        let settings = Settings {
            asr_model: SmallQ4,
            asr_model_picked: true,
            ..Settings::default()
        };

        assert_eq!(settings.speech_model_among(|model| model == Large), SmallQ4);
    }

    #[test]
    fn the_chatgpt_tokens_go_to_the_keychain_and_never_into_the_file() {
        let (dir, store) = store();
        let keychain = Arc::new(secrets::MemorySecrets::default());
        let store = store.with_secrets(Some(keychain.clone()));
        let signed_in = ChatGpt {
            host_id: "urn:uuid:x".to_string(),
            tokens: Some(Tokens {
                access_token: "access-secret".to_string(),
                refresh_token: "refresh-secret".to_string(),
                expires_at: 1,
            }),
            ..ChatGpt::default()
        };

        store.save_chatgpt(&signed_in).unwrap();

        let file = std::fs::read_to_string(dir.path().join("chatgpt.json")).unwrap();
        assert!(!file.contains("secret") && file.contains("urn:uuid:x"), "{file}");
        assert_eq!(store.chatgpt(), signed_in);
        assert_eq!(store.chatgpt_without_tokens().tokens, None);

        store.save_chatgpt(&ChatGpt { tokens: None, ..signed_in }).unwrap();
        assert!(keychain.values.lock().unwrap().is_empty());
        assert_eq!(store.chatgpt().tokens, None);
    }

    #[test]
    fn without_a_keychain_the_chatgpt_tokens_stay_in_their_own_file() {
        let (_dir, store) = store();
        let signed_in = ChatGpt {
            tokens: Some(Tokens {
                access_token: "a".to_string(),
                refresh_token: "r".to_string(),
                expires_at: 1,
            }),
            ..ChatGpt::default()
        };

        store.save_chatgpt(&signed_in).unwrap();

        assert_eq!(store.chatgpt(), signed_in);
        assert_eq!(store.chatgpt_without_tokens().tokens, None);
    }

    #[test]
    fn what_is_read_every_second_never_holds_a_secret() {
        let (_dir, store) = store();
        store.save_settings(&secret_settings()).unwrap();

        let settings = store.settings_without_secrets();

        assert!(settings.llm_api_key.is_empty() && settings.email.password.is_empty() && settings.asr_service_key.is_empty());
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
    fn an_untouched_prompt_moves_on_with_the_app_and_an_edited_one_stays() {
        let (_dir, store) = store();
        let mut settings = Settings::default();
        settings.system_prompt = PREVIOUS_SYSTEM_PROMPTS[0].to_string();
        store.save_settings(&settings).unwrap();
        assert_eq!(store.settings().system_prompt, *DEFAULT_SYSTEM_PROMPT);

        settings.system_prompt = PREVIOUS_SYSTEM_PROMPTS[0].replace("Output Markdown only", "Output plain text only");
        store.save_settings(&settings).unwrap();
        assert!(store.settings().system_prompt.contains("Output plain text only"));
    }

    #[test]
    fn unfinished_work_is_marked_failed_on_the_next_start() {
        let (_dir, store) = store();
        let mut done = store.create_meeting(at(9), "discussion").unwrap();
        done.status = Status::Done;
        store.save_meeting(&done).unwrap();
        let recording = store.create_meeting(at(10), "discussion").unwrap();
        let mut transcribed = store.create_meeting(at(11), "business").unwrap();
        transcribed.status = Status::Summarizing;
        transcribed.has_transcript = true;
        store.save_meeting(&transcribed).unwrap();

        // Only the minutes were cut short there: the transcript stands, the minutes are owed.
        assert_eq!(store.mark_interrupted(), [transcribed.id.clone()]);
        let owed = store.meeting(&transcribed.id).unwrap();
        assert_eq!(owed.status, Status::Done);
        assert!(owed.error.unwrap().contains("Minutes were not written"));

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
