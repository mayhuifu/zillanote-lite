#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use engine::pipeline::{Pipeline, Readiness};
use engine::recorder::Recording;
use engine::store::{Meeting, Settings, Status, Store};
use engine::templates::{DEFAULT_SYSTEM_PROMPT, TEMPLATES, Template};
use tauri::{AppHandle, Emitter, Manager, State};

const MEETING_UPDATED: &str = "meeting-updated";
const RECORDING_LEVEL: &str = "recording-level";

struct Active {
    meeting_id: String,
    recording: Recording,
}

struct App {
    pipeline: Pipeline,
    active: Mutex<Option<Active>>,
    /// One background job at a time: there is one recognizer and it is memory hungry.
    jobs: Arc<tokio::sync::Mutex<()>>,
}

impl App {
    fn store(&self) -> &Store {
        &self.pipeline.store
    }
}

#[derive(serde::Serialize)]
struct MeetingDetail {
    meeting: Meeting,
    minutes: Option<String>,
    transcript: Option<String>,
}

#[derive(serde::Serialize)]
struct RecordingState {
    meeting_id: String,
    elapsed_seconds: f64,
}

#[tauri::command]
fn start_recording(app: AppHandle, state: State<'_, App>) -> Result<Meeting, String> {
    let mut active = state.active.lock().map_err(|e| e.to_string())?;
    if active.is_some() {
        return Err("A recording is already running.".to_string());
    }

    let template = state.store().settings().default_template;
    let meeting = state.store().create_meeting(chrono::Local::now(), &template)?;
    let levels = app.clone();
    let recording = Recording::start(state.store().audio_path(&meeting.id), move |level| {
        let _ = levels.emit(RECORDING_LEVEL, level);
    });

    match recording {
        Ok(recording) => {
            *active = Some(Active {
                meeting_id: meeting.id.clone(),
                recording,
            });
            Ok(meeting)
        }
        Err(error) => {
            // Nothing was recorded, so nothing is worth keeping.
            let _ = state.store().delete_meeting(&meeting.id);
            Err(error)
        }
    }
}

#[tauri::command]
fn stop_recording(app: AppHandle, state: State<'_, App>) -> Result<Meeting, String> {
    let active = state
        .active
        .lock()
        .map_err(|e| e.to_string())?
        .take()
        .ok_or_else(|| "No recording is running.".to_string())?;

    let summary = active.recording.stop();
    let mut meeting = state.store().meeting(&active.meeting_id)?;
    match summary {
        Ok(summary) => {
            meeting.duration_seconds = summary.duration_seconds;
            meeting.status = Status::Transcribing;
            state.store().save_meeting(&meeting)?;
            process_in_background(&app, &state, meeting.id.clone(), None);
        }
        Err(error) => {
            meeting.status = Status::Failed;
            meeting.error = Some(error);
            state.store().save_meeting(&meeting)?;
        }
    }
    Ok(meeting)
}

#[tauri::command]
fn recording_state(state: State<'_, App>) -> Option<RecordingState> {
    let active = state.active.lock().ok()?;
    active.as_ref().map(|active| RecordingState {
        meeting_id: active.meeting_id.clone(),
        elapsed_seconds: active.recording.elapsed().as_secs_f64(),
    })
}

#[tauri::command]
fn list_meetings(state: State<'_, App>) -> Vec<Meeting> {
    state.store().meetings()
}

#[tauri::command]
fn get_meeting(state: State<'_, App>, id: String) -> Result<MeetingDetail, String> {
    Ok(MeetingDetail {
        meeting: state.store().meeting(&id)?,
        minutes: state.store().minutes(&id),
        transcript: state.store().transcript(&id).map(|transcript| transcript.to_text()),
    })
}

#[tauri::command]
fn rename_meeting(state: State<'_, App>, id: String, title: String) -> Result<Meeting, String> {
    let mut meeting = state.store().meeting(&id)?;
    let title = title.trim();
    if !title.is_empty() {
        meeting.title = title.to_string();
        state.store().save_meeting(&meeting)?;
    }
    Ok(meeting)
}

#[tauri::command]
fn delete_meeting(state: State<'_, App>, id: String) -> Result<(), String> {
    let recording_it = state
        .active
        .lock()
        .map_err(|e| e.to_string())?
        .as_ref()
        .is_some_and(|active| active.meeting_id == id);
    if recording_it {
        return Err("Stop the recording first.".to_string());
    }
    state.store().delete_meeting(&id)
}

/// Transcribes the recording again and writes fresh minutes.
#[tauri::command]
fn process_meeting(app: AppHandle, state: State<'_, App>, id: String) -> Result<(), String> {
    state.store().meeting(&id)?;
    process_in_background(&app, &state, id, None);
    Ok(())
}

/// Writes the minutes again from the existing transcript.
#[tauri::command]
fn rewrite_minutes(
    app: AppHandle,
    state: State<'_, App>,
    id: String,
    template: String,
) -> Result<(), String> {
    state.store().meeting(&id)?;
    process_in_background(&app, &state, id, Some(template));
    Ok(())
}

#[tauri::command]
fn reveal_meeting(state: State<'_, App>, id: String) -> Result<(), String> {
    state.store().meeting(&id)?;
    let dir = state.store().meeting_dir(&id);
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener)
        .arg(dir)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn get_settings(state: State<'_, App>) -> Settings {
    state.store().settings()
}

#[tauri::command]
fn save_settings(state: State<'_, App>, settings: Settings) -> Result<(), String> {
    state.store().save_settings(&settings)
}

#[tauri::command]
fn default_system_prompt() -> &'static str {
    DEFAULT_SYSTEM_PROMPT
}

#[tauri::command]
fn templates() -> &'static [Template] {
    TEMPLATES
}

#[tauri::command]
fn readiness(state: State<'_, App>) -> Readiness {
    let readiness = state.pipeline.readiness();
    // The page asks for this last while starting, so this line also says the page loaded.
    tracing::info!(
        model_found = readiness.model_found,
        server_found = readiness.server_found,
        llm_configured = readiness.llm_configured,
        "readiness_checked"
    );
    readiness
}

fn process_in_background(app: &AppHandle, state: &App, id: String, template: Option<String>) {
    let app = app.clone();
    let pipeline = state.pipeline.clone();
    let jobs = state.jobs.clone();

    tauri::async_runtime::spawn(async move {
        let _turn = jobs.lock().await;
        let on_update = {
            let app = app.clone();
            move |meeting: &Meeting| {
                let _ = app.emit(MEETING_UPDATED, meeting);
            }
        };
        match template {
            Some(template) => pipeline.rewrite_minutes(&id, &template, &on_update).await,
            None => pipeline.process(&id, &on_update).await,
        }
    });
}

fn bundled_server_dirs(app: &AppHandle) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(resources) = app.path().resource_dir() {
        dirs.push(resources.join("llama-server"));
    }
    // `cargo run` has no bundle; read the folder the fetch script fills.
    #[cfg(debug_assertions)]
    dirs.push(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/llama-server"));
    dirs
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tauri::Builder::default()
        .setup(|app| {
            let store = Store::open_default()?;
            store.mark_interrupted();
            app.manage(App {
                pipeline: Pipeline {
                    store,
                    bundled_server_dirs: bundled_server_dirs(app.handle()),
                },
                active: Mutex::new(None),
                jobs: Arc::new(tokio::sync::Mutex::new(())),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_recording,
            stop_recording,
            recording_state,
            list_meetings,
            get_meeting,
            rename_meeting,
            delete_meeting,
            process_meeting,
            rewrite_minutes,
            reveal_meeting,
            get_settings,
            save_settings,
            default_system_prompt,
            templates,
            readiness,
        ])
        .run(tauri::generate_context!())
        .expect("ZillaNote could not start");
}
