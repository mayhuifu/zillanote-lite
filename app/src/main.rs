#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use engine::download::{self, Retry};
use engine::pipeline::{Pipeline, Readiness};
use engine::recorder::Recording;
use engine::store::{Meeting, Settings, Status, Store};
use engine::templates::{DEFAULT_SYSTEM_PROMPT, TEMPLATES, Template};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, State, WindowEvent};
use tauri_plugin_dialog::DialogExt;

const MEETING_UPDATED: &str = "meeting-updated";
const RECORDING_LEVEL: &str = "recording-level";
/// Carries the running recording, or nothing once it stops, to both windows.
const RECORDING_CHANGED: &str = "recording-changed";
/// Something the user should read that is not tied to one meeting.
const NOTICE: &str = "notice";
/// Carries a [`DownloadStatus`] while the models are fetched, and once more at the end.
const DOWNLOAD_PROGRESS: &str = "download-progress";

const MAIN: &str = "main";
const MINI: &str = "mini";

struct Active {
    meeting_id: String,
    recording: Recording,
}

struct App {
    pipeline: Pipeline,
    active: Mutex<Option<Active>>,
    /// One background job at a time: there is one recognizer and it is memory hungry.
    jobs: Arc<tokio::sync::Mutex<()>>,
    download: Arc<Download>,
    /// The menu-bar item that starts and stops a recording, so its words can follow.
    record_item: MenuItem<tauri::Wry>,
}

#[derive(Default)]
struct Download {
    running: AtomicBool,
    cancel: AtomicBool,
    status: Mutex<DownloadStatus>,
}

#[derive(Clone, Default, serde::Serialize)]
struct DownloadStatus {
    running: bool,
    done_bytes: u64,
    total_bytes: u64,
    error: Option<String>,
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
    speakers: Vec<SpeakerView>,
}

/// A speaker as the window needs it: what the voice sounds like stays in the engine.
#[derive(serde::Serialize)]
struct SpeakerView {
    index: usize,
    label: String,
    named: bool,
    seconds: f64,
}

#[derive(serde::Serialize)]
struct VoiceView {
    id: String,
    name: String,
}

fn meeting_detail(store: &Store, id: &str) -> Result<MeetingDetail, String> {
    Ok(MeetingDetail {
        meeting: store.meeting(id)?,
        minutes: store.minutes(id),
        transcript: store.transcript(id).map(|transcript| transcript.to_text()),
        speakers: store
            .speakers(id)
            .into_iter()
            .map(|speaker| SpeakerView {
                index: speaker.index,
                named: speaker.voice_id.is_some(),
                label: speaker.label,
                seconds: speaker.seconds,
            })
            .collect(),
    })
}

#[derive(Clone, serde::Serialize)]
struct RecordingState {
    meeting_id: String,
    elapsed_seconds: f64,
}

#[tauri::command]
fn start_recording(app: AppHandle) -> Result<Meeting, String> {
    start(&app)
}

#[tauri::command]
fn stop_recording(app: AppHandle) -> Result<Meeting, String> {
    stop(&app)
}

fn start(app: &AppHandle) -> Result<Meeting, String> {
    let state = app.state::<App>();
    let mut active = state.active.lock().map_err(|e| e.to_string())?;
    if active.is_some() {
        return Err("A recording is already running.".to_string());
    }

    let settings = state.store().settings();
    let meeting = state.store().create_meeting(chrono::Local::now(), &settings.default_template)?;
    let levels = app.clone();
    let recording = Recording::start(
        state.store().audio_path(&meeting.id),
        settings.record_system_audio,
        move |level| {
            let _ = levels.emit(RECORDING_LEVEL, level);
        },
    );

    match recording {
        Ok(recording) => {
            if let Some(problem) = recording.system_audio_problem() {
                let _ = app.emit(NOTICE, format!("Recording the microphone only. {problem}"));
            }
            *active = Some(Active {
                meeting_id: meeting.id.clone(),
                recording,
            });
            let _ = app.emit(
                RECORDING_CHANGED,
                Some(RecordingState {
                    meeting_id: meeting.id.clone(),
                    elapsed_seconds: 0.0,
                }),
            );
            let _ = app.emit(MEETING_UPDATED, &meeting);
            let _ = state.record_item.set_text("Stop recording");
            Ok(meeting)
        }
        Err(error) => {
            // Nothing was recorded, so nothing is worth keeping.
            let _ = state.store().delete_meeting(&meeting.id);
            Err(error)
        }
    }
}

fn stop(app: &AppHandle) -> Result<Meeting, String> {
    let state = app.state::<App>();
    let active = state
        .active
        .lock()
        .map_err(|e| e.to_string())?
        .take()
        .ok_or_else(|| "No recording is running.".to_string())?;

    let summary = active.recording.stop();
    let _ = app.emit(RECORDING_CHANGED, None::<RecordingState>);
    let _ = state.record_item.set_text("Start recording");
    let mut meeting = state.store().meeting(&active.meeting_id)?;
    match summary {
        Ok(summary) => {
            meeting.duration_seconds = summary.duration_seconds;
            meeting.status = Status::Transcribing;
            state.store().save_meeting(&meeting)?;
            process_in_background(app, &state, meeting.id.clone(), None);
        }
        Err(error) => {
            meeting.status = Status::Failed;
            meeting.error = Some(error);
            state.store().save_meeting(&meeting)?;
        }
    }
    let _ = app.emit(MEETING_UPDATED, &meeting);
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
    meeting_detail(state.store(), &id)
}

/// Names one of the meeting's speakers, and remembers the voice under that name. An empty
/// name takes the name off again.
#[tauri::command]
fn name_speaker(state: State<'_, App>, id: String, index: usize, name: String) -> Result<MeetingDetail, String> {
    state.pipeline.name_speaker(&id, index, &name)?;
    meeting_detail(state.store(), &id)
}

#[tauri::command]
fn list_voices(state: State<'_, App>) -> Vec<VoiceView> {
    let voices = state.store().voices().into_iter();
    voices.map(|voice| VoiceView { id: voice.id, name: voice.name }).collect()
}

#[tauri::command]
fn forget_voice(state: State<'_, App>, id: String) -> Result<(), String> {
    state.pipeline.forget_voice(&id)
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
    state.pipeline.readiness()
}

/// Sends the meeting's minutes to the fixed address again.
#[tauri::command]
fn send_minutes(app: AppHandle, state: State<'_, App>, id: String) -> Result<(), String> {
    state.store().meeting(&id)?;
    let pipeline = state.pipeline.clone();
    tauri::async_runtime::spawn(async move {
        pipeline
            .email_minutes(&id, &move |meeting: &Meeting| {
                let _ = app.emit(MEETING_UPDATED, meeting);
            })
            .await;
    });
    Ok(())
}

/// Tries the mail settings as typed, before they are saved.
#[tauri::command]
async fn send_test_email(settings: Settings) -> Result<(), String> {
    if !settings.email.is_configured() {
        return Err("Fill in the address to send to, the sending account and its password.".to_string());
    }
    engine::email::send_test(&settings.email).await
}

/// Makes a meeting from a recording made elsewhere: the one at `path`, or the one the user
/// picks when there is none. Nothing picked is not an error.
#[tauri::command]
async fn import_recording(app: AppHandle, path: Option<String>) -> Result<Option<Meeting>, String> {
    let source = match path {
        Some(path) => PathBuf::from(path),
        None => {
            let picked = app
                .dialog()
                .file()
                .set_title("Import a recording")
                .add_filter("Recordings", engine::import::EXTENSIONS)
                .blocking_pick_file();
            match picked.and_then(|file| file.into_path().ok()) {
                Some(path) => path,
                None => return Ok(None),
            }
        }
    };

    let pipeline = app.state::<App>().pipeline.clone();
    // Converting an hour of audio takes a moment; keep it off the async threads.
    let meeting = tauri::async_runtime::spawn_blocking(move || pipeline.import(&source))
        .await
        .map_err(|e| e.to_string())??;
    let _ = app.emit(MEETING_UPDATED, &meeting);
    process_in_background(&app, &app.state::<App>(), meeting.id.clone(), None);
    Ok(Some(meeting))
}

/// Fetches the models that are not on this machine yet, in the background.
#[tauri::command]
fn download_models(app: AppHandle, state: State<'_, App>) -> Result<(), String> {
    let shared = state.download.clone();
    if shared.running.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    shared.cancel.store(false, Ordering::SeqCst);
    let store = state.store().clone();
    // Known to be running from this moment, not from the first byte: a page that asks in
    // between must not offer to start it again.
    let beginning = DownloadStatus {
        running: true,
        ..DownloadStatus::default()
    };
    if let Ok(mut status) = shared.status.lock() {
        *status = beginning.clone();
    }
    let _ = app.emit(DOWNLOAD_PROGRESS, beginning);

    tauri::async_runtime::spawn(async move {
        let packages = download::missing_packages(&store.models_dir(), &store.speaker_models_dir());
        let publish = |status: DownloadStatus| {
            *shared.status.lock().unwrap() = status.clone();
            let _ = app.emit(DOWNLOAD_PROGRESS, status);
        };
        let last = Mutex::new(Instant::now() - Duration::from_secs(1));
        let on_progress = |progress: download::Progress| {
            let mut last = last.lock().unwrap();
            if last.elapsed() >= Duration::from_millis(200) {
                *last = Instant::now();
                publish(DownloadStatus {
                    running: true,
                    done_bytes: progress.done_bytes,
                    total_bytes: progress.total_bytes,
                    error: None,
                });
            }
        };

        let result = download::download(&packages, Retry::default(), &shared.cancel, &on_progress).await;
        let total_bytes = download::total_bytes(&packages);
        if result.is_ok() {
            // The recordings that were waiting for the speech model get their turn now.
            let state = app.state::<App>();
            for id in state.pipeline.waiting_for_model() {
                process_in_background(&app, &state, id, None);
            }
        }
        let stopped = result.as_ref().is_err_and(|error| error == download::CANCELLED);
        shared.running.store(false, Ordering::SeqCst);
        // Read before publishing, which takes the same lock.
        let arrived = shared.status.lock().unwrap().done_bytes;
        publish(DownloadStatus {
            running: false,
            done_bytes: if result.is_ok() { total_bytes } else { arrived },
            total_bytes,
            // Stopping is the user's own doing, not a problem to report.
            error: result.err().filter(|_| !stopped),
        });
    });
    Ok(())
}

#[tauri::command]
fn cancel_download(state: State<'_, App>) {
    state.download.cancel.store(true, Ordering::SeqCst);
}

#[tauri::command]
fn download_status(state: State<'_, App>) -> DownloadStatus {
    state.download.status.lock().map(|status| status.clone()).unwrap_or_default()
}

/// Where macOS lists the apps allowed to record the computer's sound.
#[tauri::command]
fn open_system_audio_settings() -> Result<(), String> {
    std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Each page calls this once its script has run to the end, so a page that failed to start
/// shows up as a missing line in the log.
#[tauri::command]
fn ui_ready(window: tauri::WebviewWindow) {
    tracing::info!(window = window.label(), "ui_ready");
}

/// The full window, in place of the bar.
#[tauri::command]
fn show_main(app: AppHandle) -> Result<(), String> {
    let main = app.get_webview_window(MAIN).ok_or("The main window is missing.")?;
    main.show().map_err(|e| e.to_string())?;
    let _ = main.unminimize();
    let _ = main.set_focus();
    if let Some(mini) = app.get_webview_window(MINI) {
        let _ = mini.hide();
    }
    Ok(())
}

/// Back to the bar.
#[tauri::command]
fn show_mini(app: AppHandle) -> Result<(), String> {
    let mini = app.get_webview_window(MINI).ok_or("The bar is missing.")?;
    mini.show().map_err(|e| e.to_string())?;
    if let Some(main) = app.get_webview_window(MAIN) {
        let _ = main.hide();
    }
    Ok(())
}

/// The bar starts at the right edge of the main screen, half way down.
fn place_mini(app: &AppHandle) {
    let Some(mini) = app.get_webview_window(MINI) else {
        return;
    };
    let (Ok(Some(monitor)), Ok(size)) = (mini.primary_monitor(), mini.outer_size()) else {
        return;
    };
    let margin = (16.0 * monitor.scale_factor()) as i32;
    let x = monitor.position().x + monitor.size().width as i32 - size.width as i32 - margin;
    let y = monitor.position().y + (monitor.size().height as i32 - size.height as i32) / 2;
    let _ = mini.set_position(PhysicalPosition::new(x, y));
}

/// The icon in the menu bar: recording without looking for the window, and the way out.
fn build_tray(app: &AppHandle) -> tauri::Result<MenuItem<tauri::Wry>> {
    let record = MenuItem::with_id(app, "record", "Start recording", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &record,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "open", "Open ZillaNote", true, None::<&str>)?,
            &MenuItem::with_id(app, "import", "Import a recording…", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "quit", "Quit ZillaNote", true, None::<&str>)?,
        ],
    )?;

    TrayIconBuilder::with_id("tray")
        .icon(tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?)
        .icon_as_template(true)
        .tooltip("ZillaNote")
        .menu(&menu)
        .on_menu_event(|app, event| {
            let recording = || app.state::<App>().active.lock().is_ok_and(|active| active.is_some());
            let result = match event.id().as_ref() {
                "record" if recording() => stop(app).map(|_| ()),
                "record" => start(app).map(|_| ()),
                "open" => show_main(app.clone()),
                "import" => {
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(error) = import_recording(app.clone(), None).await {
                            // The reason is shown in the window, so the window has to be there.
                            let _ = show_main(app.clone());
                            let _ = app.emit(NOTICE, error);
                        }
                    });
                    Ok(())
                }
                "quit" => {
                    // A recording under way is closed properly first: the audio is what matters.
                    if recording() {
                        let _ = stop(app);
                    }
                    app.exit(0);
                    Ok(())
                }
                _ => Ok(()),
            };
            if let Err(error) = result {
                let _ = show_main(app.clone());
                let _ = app.emit(NOTICE, error);
            }
        })
        .build(app)?;
    Ok(record)
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
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let store = Store::open_default()?;
            store.mark_interrupted();
            store.migrate_secrets();
            let record_item = build_tray(app.handle())?;
            app.manage(App {
                pipeline: Pipeline {
                    store,
                    bundled_server_dirs: bundled_server_dirs(app.handle()),
                },
                active: Mutex::new(None),
                jobs: Arc::new(tokio::sync::Mutex::new(())),
                download: Arc::default(),
                record_item,
            });
            place_mini(app.handle());
            Ok(())
        })
        // Closing the full window goes back to the bar; the app keeps running.
        .on_window_event(|window, event| {
            if window.label() == MAIN
                && let WindowEvent::CloseRequested { api, .. } = event
            {
                api.prevent_close();
                let _ = show_mini(window.app_handle().clone());
            }
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
            show_main,
            show_mini,
            ui_ready,
            send_minutes,
            send_test_email,
            open_system_audio_settings,
            name_speaker,
            list_voices,
            forget_voice,
            download_models,
            cancel_download,
            download_status,
            import_recording,
        ])
        .run(tauri::generate_context!())
        .expect("ZillaNote could not start");
}
