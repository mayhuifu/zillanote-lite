#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use engine::calls::{CallState, CallWatch};
use engine::download::{self, Retry};
use engine::pipeline::{Pipeline, Readiness};
use engine::recorder::Recording;
use engine::store::{AsrProvider, Meeting, Settings, Status, Store};
use engine::templates::{DEFAULT_SYSTEM_PROMPT, TEMPLATES, Template};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, RunEvent, State, WindowEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

const MEETING_UPDATED: &str = "meeting-updated";
const RECORDING_LEVEL: &str = "recording-level";
/// Carries the running recording, or nothing once it stops, to both windows.
const RECORDING_CHANGED: &str = "recording-changed";
/// Something the user should read that is not tied to one meeting.
const NOTICE: &str = "notice";
/// Carries a [`DownloadStatus`] while the models are fetched, and once more at the end.
const DOWNLOAD_PROGRESS: &str = "download-progress";
/// Carries a [`CallState`] whenever it changes: a call program opened the microphone, let
/// it go, or the countdown to the end of the recording moved on.
const CALL_STATE: &str = "call-state";

/// The Quit of the menu at the top of the screen (the menu-bar icon has a Quit of its own).
const APP_QUIT: &str = "app-quit";

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
    /// Who else holds the microphone, read once a second by [`watch_calls`].
    calls: Mutex<CallWatch>,
    /// What the windows were last told about calls, for a window that opens later.
    call_state: Mutex<CallState>,
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
    stop_and(app, true)
}

/// Closes the recording properly. With `process`, it goes on to be transcribed; without
/// (the app is leaving), it is kept for the next time.
fn stop_and(app: &AppHandle, process: bool) -> Result<Meeting, String> {
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
        Ok(summary) if process => {
            meeting.duration_seconds = summary.duration_seconds;
            meeting.status = Status::Transcribing;
            state.store().save_meeting(&meeting)?;
            process_in_background(app, &state, meeting.id.clone(), None);
        }
        Ok(summary) => {
            meeting.duration_seconds = summary.duration_seconds;
            meeting.status = Status::Failed;
            meeting.error = Some("ZillaNote was closed before this was transcribed. Choose Transcribe again.".to_string());
            state.store().save_meeting(&meeting)?;
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

/// Shown after the name in the window, so a report can say which build it is about.
#[tauri::command]
fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Once a second: who holds the microphone, and what follows from it. A call program
/// opening it makes the bar flash; one letting it go during a recording starts a countdown
/// at whose end the recording stops as if the button had been pressed.
fn watch_calls(app: AppHandle) {
    let mut last_users = Vec::new();
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let state = app.state::<App>();
        let settings = state.store().settings_without_secrets();
        let users = if settings.notice_calls || settings.stop_when_call_ends {
            engine::calls::microphone_users()
        } else {
            Vec::new()
        };
        if users != last_users {
            tracing::info!(holding = ?users.iter().map(|user| &user.name).collect::<Vec<_>>(), "microphone_holders");
            last_users = users.clone();
        }
        let recording = state.active.lock().map(|active| active.is_some()).unwrap_or(false);
        let tick = match state.calls.lock() {
            Ok(mut calls) => calls.observe(Instant::now(), &users, recording, settings.stop_when_call_ends),
            Err(_) => continue,
        };
        let mut shown = tick.state;
        if !settings.notice_calls && shown.ending_in.is_none() {
            shown.app = None;
        }
        if tick.stop {
            let app_name = last_call_name(&state).unwrap_or_else(|| "The call".to_string());
            match stop(&app) {
                Ok(_) => {
                    tracing::info!(call = %app_name, "recording_stopped_after_call");
                    let _ = app.emit(NOTICE, format!("Recording stopped: the {app_name} call ended."));
                }
                Err(error) => tracing::warn!(%error, "recording_stop_after_call_failed"),
            }
        }
        let changed = state.call_state.lock().map(|mut held| {
            let changed = *held != shown;
            if changed {
                if held.app.is_none() && shown.app.is_some() {
                    tracing::info!(call = shown.app.as_deref().unwrap_or(""), "call_started");
                } else if held.app.is_some() && shown.app.is_none() && !tick.stop {
                    tracing::info!(call = held.app.as_deref().unwrap_or(""), "call_over");
                }
                *held = shown.clone();
            }
            changed
        });
        if changed.unwrap_or(false) {
            let _ = app.emit(CALL_STATE, &shown);
        }
    }
}

/// The name the countdown was shown under, for the notice when it runs out.
fn last_call_name(state: &App) -> Option<String> {
    state.call_state.lock().ok()?.app.clone()
}

/// What the windows show about calls right now, for a window that opens later.
#[tauri::command]
fn call_state(state: State<'_, App>) -> CallState {
    state.call_state.lock().map(|held| held.clone()).unwrap_or_default()
}

/// The user wants the recording to go on although the call ended.
#[tauri::command]
fn keep_recording(app: AppHandle, state: State<'_, App>) -> Result<(), String> {
    state.calls.lock().map_err(|e| e.to_string())?.keep();
    let shown = CallState::default();
    if let Ok(mut held) = state.call_state.lock() {
        *held = shown.clone();
    }
    tracing::info!("recording_kept_after_call");
    let _ = app.emit(CALL_STATE, &shown);
    Ok(())
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
    DEFAULT_SYSTEM_PROMPT.as_str()
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
    let sent = engine::email::send_test(&settings.email).await;
    match &sent {
        Ok(()) => tracing::info!(to = %settings.email.to.trim(), "email_test_sent"),
        Err(error) => tracing::warn!(%error, "email_test_failed"),
    }
    sent
}

/// Tries the speech service as typed, before it is saved: a second of sound, and what the
/// service made of it.
#[tauri::command]
async fn test_speech_service(settings: Settings) -> Result<String, String> {
    let answer = engine::pipeline::test_speech_service(&settings).await;
    match &answer {
        Ok(_) => tracing::info!(service = %settings.asr_service_url.trim(), "speech_service_test_answered"),
        Err(error) => tracing::warn!(%error, "speech_service_test_failed"),
    }
    answer
}

/// Deletes a speech model's files of ZillaNote's own to give the space back, and says how
/// many bytes that was. Not while the recognizer may have them open, or while they arrive.
#[tauri::command]
fn delete_model(state: State<'_, App>, id: String) -> Result<u64, String> {
    if state.download.running.load(Ordering::SeqCst) {
        return Err("A download is running. Stop it or let it finish, then delete the model.".to_string());
    }
    // Held for as long as the job runs, so nothing can start transcribing meanwhile.
    let _no_job = state
        .jobs
        .try_lock()
        .map_err(|_| "A meeting is being worked on. Delete the model when it is done.".to_string())?;
    state.pipeline.delete_model(&id)
}

/// Leaves the app. When that would cut something short, it asks first, and says what
/// happens to the work; a recording under way is closed properly, never dropped.
async fn request_quit(app: AppHandle) {
    let (recording, working) = {
        let state = app.state::<App>();
        let recording = state.active.lock().is_ok_and(|active| active.is_some());
        (recording, state.jobs.try_lock().is_err())
    };
    if recording || working {
        let message = if recording {
            "A recording is running. It will be stopped and kept: choose Transcribe again on it the next time you open ZillaNote."
        } else {
            "A meeting is still being worked on. Its recording is safe: choose Transcribe again on it the next time you open ZillaNote."
        };
        let dialog = app
            .dialog()
            .message(message)
            .title("Quit ZillaNote?")
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom("Quit".to_string(), "Keep running".to_string()));
        // The dialog waits for the answer, which the main thread must not do.
        let confirmed = tauri::async_runtime::spawn_blocking(move || dialog.blocking_show()).await;
        if !confirmed.unwrap_or(false) {
            return;
        }
    }
    close_down(&app);
    app.exit(0);
}

/// What has to happen whichever way the app goes: the recording closed into a whole file,
/// and the recognizer, which holds gigabytes, not left running behind.
fn close_down(app: &AppHandle) {
    let recording = app.state::<App>().active.lock().is_ok_and(|active| active.is_some());
    if recording {
        let _ = stop_and(app, false);
    }
    engine::pipeline::stop_servers();
}

#[tauri::command]
async fn quit_app(app: AppHandle) {
    request_quit(app).await;
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
        let settings = store.settings();
        // A speech service needs no speech model here; the speaker models are still wanted.
        let speech_model = (settings.asr_provider == AsrProvider::Local).then_some(settings.asr_model);
        let packages = download::missing_packages(&store.models_dir(), &store.speaker_models_dir(), speech_model);
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

/// Bytes a second this connection fetches model files at, for the estimates in Settings.
#[tauri::command]
async fn probe_download_speed() -> Option<f64> {
    download::probe_speed().await
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

/// Windows makes no window with a title bar narrower than its own smallest width (136
/// pixels at 100%), and to Windows the bar has one, hidden: it came out several times as
/// wide as it was drawn. A smallest size of our own lifts that limit, but only for a window
/// that is already there, so the size is asked for a second time. The app's menu comes off
/// first: on Windows it is a row inside every window, which the bar never shows and would
/// still be measured with.
fn narrow_mini(app: &AppHandle, mini: &tauri::WebviewWindow) {
    let Some(drawn) = app.config().app.windows.iter().find(|window| window.label == MINI) else {
        return;
    };
    let size = LogicalSize::new(drawn.width, drawn.height);
    let _ = mini.remove_menu();
    let _ = mini.set_min_size(Some(size));
    let _ = mini.set_size(size);
}

/// The bar starts at the right edge of the main screen, half way down.
fn place_mini(app: &AppHandle) {
    let Some(mini) = app.get_webview_window(MINI) else {
        return;
    };
    if cfg!(windows) {
        narrow_mini(app, &mini);
    }
    if let (Ok(size), Ok(scale)) = (mini.inner_size(), mini.scale_factor()) {
        let size = size.to_logical::<f64>(scale);
        tracing::info!(width = size.width.round(), height = size.height.round(), scale, "bar_size");
    }
    let (Ok(Some(monitor)), Ok(size)) = (mini.primary_monitor(), mini.outer_size()) else {
        return;
    };
    let margin = (16.0 * monitor.scale_factor()) as i32;
    let x = monitor.position().x + monitor.size().width as i32 - size.width as i32 - margin;
    let y = monitor.position().y + (monitor.size().height as i32 - size.height as i32) / 2;
    let _ = mini.set_position(PhysicalPosition::new(x, y));
}

/// The menu at the top of the screen. Quit is ours rather than the system's, so that ⌘Q
/// takes the same careful way out as every other Quit; Edit has to be there for copy and
/// paste to work in the window's text fields.
fn build_app_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let application = Submenu::with_items(
        app,
        "ZillaNote",
        true,
        &[
            &PredefinedMenuItem::about(app, None, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, None)?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::show_all(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, APP_QUIT, "Quit ZillaNote", true, Some("CmdOrCtrl+Q"))?,
        ],
    )?;
    let edit = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;
    let window = Submenu::with_items(
        app,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::close_window(app, None)?,
        ],
    )?;
    Menu::with_items(app, &[&application, &edit, &window])
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

    // macOS tints a template image to suit the menu bar. Anywhere else a black mark would
    // vanish on a dark taskbar, so the app's own icon goes there.
    let tray = TrayIconBuilder::with_id("tray");
    #[cfg(target_os = "macos")]
    let tray = tray
        .icon(tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?)
        .icon_as_template(true);
    #[cfg(not(target_os = "macos"))]
    let tray = match app.default_window_icon() {
        Some(icon) => tray.icon(icon.clone()),
        None => tray,
    };
    tray.tooltip("ZillaNote")
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
                    tauri::async_runtime::spawn(request_quit(app.clone()));
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

/// What the app did goes to the terminal, if there is one, and to `zillanote.log` in the
/// data folder: started from the Dock there is no terminal, and "why were there no minutes
/// at half past three" should still have an answer. The file starts over once it is large.
fn init_logging() {
    use tracing_subscriber::fmt::writer::MakeWriterExt;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let file = Store::open_default().ok().and_then(|store| {
        let path = store.log_path();
        let large = std::fs::metadata(&path).is_ok_and(|log| log.len() > 2_000_000);
        std::fs::OpenOptions::new().create(true).append(!large).write(true).truncate(large).open(path).ok()
    });
    let builder = tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false).with_timer(LocalTime);
    match file {
        Some(file) => builder.with_writer(std::io::stderr.and(Mutex::new(file))).init(),
        None => builder.init(),
    }
}

/// The clock on the wall, which is what "half past three" is measured by.
struct LocalTime;

impl tracing_subscriber::fmt::time::FormatTime for LocalTime {
    fn format_time(&self, writer: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        write!(writer, "{}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"))
    }
}

fn main() {
    init_logging();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let store = Store::open_default()?;
            let owed_minutes = store.mark_interrupted();
            store.migrate_secrets();
            tracing::info!(version = env!("CARGO_PKG_VERSION"), "started");
            let record_item = build_tray(app.handle())?;
            app.set_menu(build_app_menu(app.handle())?)?;
            app.on_menu_event(|app, event| {
                if event.id().as_ref() == APP_QUIT {
                    tauri::async_runtime::spawn(request_quit(app.clone()));
                }
            });
            app.manage(App {
                pipeline: Pipeline {
                    store,
                    bundled_server_dirs: bundled_server_dirs(app.handle()),
                },
                active: Mutex::new(None),
                jobs: Arc::new(tokio::sync::Mutex::new(())),
                download: Arc::default(),
                record_item,
                calls: Mutex::new(CallWatch::new()),
                call_state: Mutex::default(),
            });
            place_mini(app.handle());
            std::thread::spawn({
                let app = app.handle().clone();
                move || watch_calls(app)
            });
            // Meetings whose minutes were cut short by the last exit get them now.
            let state = app.state::<App>();
            if state.store().settings().auto_minutes {
                for id in owed_minutes {
                    if let Ok(meeting) = state.store().meeting(&id) {
                        process_in_background(app.handle(), &state, id, Some(meeting.template));
                    }
                }
            }
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
            app_version,
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
            call_state,
            keep_recording,
            send_minutes,
            send_test_email,
            test_speech_service,
            delete_model,
            open_system_audio_settings,
            name_speaker,
            list_voices,
            forget_voice,
            download_models,
            cancel_download,
            download_status,
            import_recording,
            quit_app,
            probe_download_speed,
        ])
        .build(tauri::generate_context!())
        .expect("ZillaNote could not start")
        // Quit from the Dock, a log-out, a shutdown: no question can be asked any more, but
        // the recording is still closed into a whole file and the recognizer stopped.
        .run(|app, event| {
            if let RunEvent::Exit = event {
                close_down(app);
            }
        });
}
