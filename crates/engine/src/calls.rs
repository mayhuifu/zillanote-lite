//! Calls: who else has the microphone open, and what that means for a recording.
//!
//! Every call program (Teams, Zoom, Tencent Meeting, a browser running Meet) holds the
//! microphone for as long as the call runs and lets it go the moment the call ends. That is
//! the whole signal: no list of programs to keep up to date, and no permission to ask for.
//! [`microphone_users`] reads it from the system; [`CallWatch`] turns a second-by-second
//! series of those readings into what the windows show and when a recording should stop.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Another program that has the microphone open right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicUser {
    pub pid: u32,
    /// The everyday name, "Zoom", not the bundle or the executable.
    pub name: String,
}

/// What the windows show about calls.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct CallState {
    /// The program on a call, or none.
    pub app: Option<String>,
    /// Seconds left until the recording stops because the call ended, while that counts down.
    pub ending_in: Option<u64>,
}

/// One second's verdict.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Tick {
    pub state: CallState,
    /// The countdown ran out: stop the recording now.
    pub stop: bool,
}

/// How long a program must hold the microphone before the bar says a meeting has started.
/// Siri's listener and a spoken command let go within a second or two.
pub const NOTICE_AFTER: Duration = Duration::from_secs(3);
/// How long a program must hold the microphone before its letting go counts as a call
/// ending. A voice message or dictation is over well within this.
pub const CALL_AFTER: Duration = Duration::from_secs(60);
/// How long the recording goes on after the call ended, for a reconnect, a headset switch,
/// or the next meeting, and for the user to say "keep recording".
pub const GRACE: Duration = Duration::from_secs(30);

struct Holder {
    name: String,
    since: Instant,
    /// Held long enough to be a call.
    counted: bool,
}

struct Ended {
    name: String,
    at: Instant,
}

/// The state machine. Feed it every second with who holds the microphone.
pub struct CallWatch {
    holding: HashMap<u32, Holder>,
    /// The last call that ended while nothing else counted as one, until the countdown
    /// runs out, the user keeps recording, or the call turns out to go on.
    ended: Option<Ended>,
}

impl Default for CallWatch {
    fn default() -> Self {
        Self::new()
    }
}

impl CallWatch {
    pub fn new() -> Self {
        Self {
            holding: HashMap::new(),
            ended: None,
        }
    }

    /// `recording` is whether a recording runs right now; `stop_when_call_ends` is the
    /// setting. With either off, a call ending changes nothing.
    pub fn observe(&mut self, now: Instant, users: &[MicUser], recording: bool, stop_when_call_ends: bool) -> Tick {
        // Who arrived, who left.
        for user in users {
            self.holding.entry(user.pid).or_insert_with(|| Holder {
                name: user.name.clone(),
                since: now,
                counted: false,
            });
        }
        let gone: Vec<u32> = self
            .holding
            .keys()
            .filter(|pid| !users.iter().any(|user| user.pid == **pid))
            .copied()
            .collect();
        for pid in gone {
            if let Some(holder) = self.holding.remove(&pid)
                && holder.counted
            {
                self.ended = Some(Ended {
                    name: holder.name,
                    at: now,
                });
            }
        }

        // Who has held long enough. While a call has just ended, a program that holds the
        // microphone for more than a moment is that call going on (a reconnect) or the next
        // one, and counts at once.
        for holder in self.holding.values_mut() {
            if holder.counted {
                continue;
            }
            let held = now.saturating_duration_since(holder.since);
            if held >= CALL_AFTER || (self.ended.is_some() && held >= NOTICE_AFTER) {
                holder.counted = true;
            }
        }
        if self.holding.values().any(|holder| holder.counted) || !recording || !stop_when_call_ends {
            self.ended = None;
        }

        let mut stop = false;
        let mut ending_in = None;
        if let Some(ended) = &self.ended {
            let elapsed = now.saturating_duration_since(ended.at);
            if elapsed >= GRACE {
                stop = true;
            } else {
                ending_in = Some((GRACE - elapsed).as_secs_f64().ceil() as u64);
            }
        }

        let app = if stop {
            None
        } else if let Some(ended) = &self.ended {
            Some(ended.name.clone())
        } else {
            self.holding
                .values()
                .filter(|holder| holder.counted || now.saturating_duration_since(holder.since) >= NOTICE_AFTER)
                .min_by_key(|holder| (holder.since, holder.name.clone()))
                .map(|holder| holder.name.clone())
        };
        if stop {
            self.ended = None;
        }

        Tick {
            state: CallState { app, ending_in },
            stop,
        }
    }

    /// The user wants the recording to go on although the call ended.
    pub fn keep(&mut self) {
        self.ended = None;
    }
}

/// The everyday name of a program, from its bundle identifier (macOS) or the file name of
/// its executable (Windows); none for the system's own helpers, which are not calls.
pub fn program_name(id: &str) -> Option<String> {
    let lower = id.to_ascii_lowercase();
    let known: &[(&str, &str)] = &[
        ("zoom", "Zoom"),
        ("com.microsoft.teams", "Teams"),
        ("ms-teams", "Teams"),
        ("teams.exe", "Teams"),
        ("com.tencent.meeting", "Tencent Meeting"),
        ("wemeet", "Tencent Meeting"),
        ("com.tencent.xinwechat", "WeChat"),
        ("wechat", "WeChat"),
        ("weixin", "WeChat"),
        ("dingtalk", "DingTalk"),
        ("com.electron.lark", "Feishu"),
        ("larksuite", "Feishu"),
        ("feishu", "Feishu"),
        ("lark.exe", "Feishu"),
        ("com.google.chrome", "Chrome"),
        ("chrome.exe", "Chrome"),
        ("com.microsoft.edgemac", "Edge"),
        ("msedge", "Edge"),
        ("org.mozilla.firefox", "Firefox"),
        ("firefox", "Firefox"),
        ("com.apple.webkit.gpu", "Safari"),
        ("com.apple.safari", "Safari"),
        ("com.apple.facetime", "FaceTime"),
        ("webex", "Webex"),
        ("ciscocollabhost", "Webex"),
        ("com.tinyspeck.slackmacgap", "Slack"),
        ("slack.exe", "Slack"),
        ("discord", "Discord"),
        ("skype", "Skype"),
    ];
    if let Some((_, name)) = known.iter().find(|(key, _)| lower.contains(key)) {
        return Some(name.to_string());
    }
    // The rest of Apple's and Windows' own processes open the microphone for Siri, dictation
    // and voice access, never for a call.
    if lower.starts_with("com.apple.") || matches!(lower.as_str(), "svchost.exe" | "audiodg.exe" | "searchhost.exe") {
        return None;
    }
    let last = lower.rsplit(['.', '\\', '/']).find(|part| !part.is_empty() && !matches!(*part, "exe" | "app" | "helper"));
    let last = match last {
        Some(part) => part,
        None => return None,
    };
    let mut chars = last.chars();
    let first = chars.next()?;
    Some(first.to_uppercase().chain(chars).collect())
}

/// The programs holding the microphone open now, other than this one and the system's own.
pub fn microphone_users() -> Vec<MicUser> {
    platform::microphone_users()
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{MicUser, program_name};
    use objc2_core_audio::{
        AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID, AudioObjectPropertyAddress,
        AudioObjectPropertySelector, kAudioHardwarePropertyProcessObjectList, kAudioObjectPropertyElementMain,
        kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, kAudioProcessPropertyBundleID,
        kAudioProcessPropertyIsRunningInput, kAudioProcessPropertyPID,
    };
    use objc2_core_foundation::{CFRetained, CFString};
    use std::ptr::NonNull;

    fn address(selector: AudioObjectPropertySelector) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress {
            mSelector: selector,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        }
    }

    /// A property that is one plain number.
    fn number<T: Copy>(object: AudioObjectID, selector: AudioObjectPropertySelector, zero: T) -> Option<T> {
        let address = address(selector);
        let mut value = zero;
        let mut size = size_of::<T>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                NonNull::from(&address),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::from(&mut value).cast(),
            )
        };
        (status == 0).then_some(value)
    }

    fn bundle_id(object: AudioObjectID) -> Option<String> {
        let address = address(kAudioProcessPropertyBundleID);
        let mut string: *const CFString = std::ptr::null();
        let mut size = size_of::<*const CFString>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                NonNull::from(&address),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::from(&mut string).cast(),
            )
        };
        if status != 0 {
            return None;
        }
        // The property hands over one reference, which is ours to release.
        let string = unsafe { CFRetained::from_raw(NonNull::new(string.cast_mut())?) };
        Some(string.to_string()).filter(|s| !s.is_empty())
    }

    /// What the process calls itself, for one without a bundle.
    fn process_name(pid: u32) -> Option<String> {
        let mut buffer = [0u8; 256];
        let length = unsafe { libc::proc_name(pid as i32, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
        (length > 0).then(|| String::from_utf8_lossy(&buffer[..length as usize]).into_owned())
    }

    fn processes() -> Vec<AudioObjectID> {
        let address = address(kAudioHardwarePropertyProcessObjectList);
        let system = kAudioObjectSystemObject as AudioObjectID;
        let mut size = 0u32;
        if unsafe { AudioObjectGetPropertyDataSize(system, NonNull::from(&address), 0, std::ptr::null(), NonNull::from(&mut size)) } != 0 {
            return Vec::new();
        }
        let mut ids = vec![0 as AudioObjectID; size as usize / size_of::<AudioObjectID>()];
        if ids.is_empty() {
            return ids;
        }
        let status = unsafe {
            AudioObjectGetPropertyData(
                system,
                NonNull::from(&address),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::new_unchecked(ids.as_mut_ptr().cast()),
            )
        };
        if status != 0 {
            return Vec::new();
        }
        ids.truncate(size as usize / size_of::<AudioObjectID>());
        ids
    }

    pub fn microphone_users() -> Vec<MicUser> {
        let me = std::process::id();
        processes()
            .into_iter()
            .filter(|object| number(*object, kAudioProcessPropertyIsRunningInput, 0u32) == Some(1))
            .filter_map(|object| {
                let pid = number(object, kAudioProcessPropertyPID, 0i32)? as u32;
                if pid == me {
                    return None;
                }
                let name = match bundle_id(object) {
                    Some(bundle) => program_name(&bundle)?,
                    None => program_name(&process_name(pid)?)?,
                };
                Some(MicUser { pid, name })
            })
            .collect()
    }
}

#[cfg(windows)]
mod platform {
    use super::{MicUser, program_name};
    use windows::Win32::Foundation::{CloseHandle, S_OK};
    use windows::Win32::Media::Audio::{
        AudioSessionStateActive, DEVICE_STATE_ACTIVE, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
        MMDeviceEnumerator, eCapture,
    };
    use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };
    use windows::core::{Interface, PWSTR};

    /// The file name of the process's executable, "Zoom.exe".
    fn executable(pid: u32) -> Option<String> {
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
        let mut buffer = [0u16; 1024];
        let mut length = buffer.len() as u32;
        let result = unsafe { QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buffer.as_mut_ptr()), &mut length) };
        let _ = unsafe { CloseHandle(handle) };
        result.ok()?;
        let path = String::from_utf16_lossy(&buffer[..length as usize]);
        path.rsplit(['\\', '/']).next().map(str::to_string)
    }

    /// Every program with a running stream on any microphone. The audio session list is per
    /// endpoint, so all of them are asked; a call may use a headset that is not the default.
    fn users() -> windows::core::Result<Vec<MicUser>> {
        let me = std::process::id();
        let mut users = Vec::new();
        unsafe {
            let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let devices = enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
            for index in 0..devices.GetCount()? {
                let device = devices.Item(index)?;
                let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
                let sessions = manager.GetSessionEnumerator()?;
                for index in 0..sessions.GetCount()? {
                    let session = sessions.GetSession(index)?;
                    if session.GetState()? != AudioSessionStateActive {
                        continue;
                    }
                    let session: IAudioSessionControl2 = session.cast()?;
                    // S_FALSE for an ordinary session, and that is "ok" too, so exactly S_OK.
                    if session.IsSystemSoundsSession() == S_OK {
                        continue;
                    }
                    let pid = session.GetProcessId()?;
                    if pid == 0 || pid == me || users.iter().any(|user: &MicUser| user.pid == pid) {
                        continue;
                    }
                    let Some(name) = executable(pid).and_then(|exe| program_name(&exe)) else {
                        continue;
                    };
                    users.push(MicUser { pid, name });
                }
            }
        }
        Ok(users)
    }

    pub fn microphone_users() -> Vec<MicUser> {
        // Once per thread; already-initialized is an answer, not a failure.
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        users().unwrap_or_default()
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    pub fn microphone_users() -> Vec<super::MicUser> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(pid: u32, name: &str) -> MicUser {
        MicUser {
            pid,
            name: name.to_string(),
        }
    }

    /// Runs the watch second by second from `start`, with `users` present throughout, and
    /// returns the last tick.
    fn run(watch: &mut CallWatch, start: Instant, seconds: u64, users: &[MicUser], recording: bool) -> Tick {
        let mut last = Tick::default();
        for second in 0..=seconds {
            last = watch.observe(start + Duration::from_secs(second), users, recording, true);
        }
        last
    }

    #[test]
    fn a_program_that_holds_the_microphone_for_a_few_seconds_is_a_meeting_starting() {
        let mut watch = CallWatch::new();
        let t0 = Instant::now();
        let zoom = [user(7, "Zoom")];
        assert_eq!(watch.observe(t0, &zoom, false, true).state.app, None, "not on the first sight");
        assert_eq!(watch.observe(t0 + Duration::from_secs(1), &zoom, false, true).state.app, None);
        assert_eq!(watch.observe(t0 + NOTICE_AFTER, &zoom, false, true).state.app.as_deref(), Some("Zoom"));
        // Gone again: Siri's listener does this all day.
        assert_eq!(watch.observe(t0 + Duration::from_secs(4), &[], false, true), Tick::default());
    }

    #[test]
    fn a_short_use_of_the_microphone_never_ends_a_recording() {
        let mut watch = CallWatch::new();
        let t0 = Instant::now();
        run(&mut watch, t0, 20, &[user(3, "Dictation")], true);
        let after = run(&mut watch, t0 + Duration::from_secs(21), 40, &[], true);
        assert_eq!(after, Tick::default());
    }

    #[test]
    fn the_call_ending_counts_down_and_then_stops_the_recording() {
        let mut watch = CallWatch::new();
        let t0 = Instant::now();
        let zoom = [user(7, "Zoom")];
        let on = run(&mut watch, t0, CALL_AFTER.as_secs() + 5, &zoom, true);
        assert_eq!(on.state, CallState { app: Some("Zoom".into()), ending_in: None });

        let t1 = t0 + Duration::from_secs(CALL_AFTER.as_secs() + 6);
        let first = watch.observe(t1, &[], true, true);
        assert_eq!(first.state, CallState { app: Some("Zoom".into()), ending_in: Some(30) });
        assert!(!first.stop);
        let later = watch.observe(t1 + Duration::from_secs(12), &[], true, true);
        assert_eq!(later.state.ending_in, Some(18));
        let last = watch.observe(t1 + GRACE, &[], true, true);
        assert!(last.stop, "the grace period is over");
        assert_eq!(last.state, CallState::default());
        // Once is enough.
        assert!(!watch.observe(t1 + GRACE + Duration::from_secs(1), &[], false, true).stop);
    }

    #[test]
    fn a_reconnect_within_the_grace_period_is_the_same_call() {
        let mut watch = CallWatch::new();
        let t0 = Instant::now();
        let teams = [user(9, "Teams")];
        run(&mut watch, t0, CALL_AFTER.as_secs(), &teams, true);
        let t1 = t0 + Duration::from_secs(CALL_AFTER.as_secs() + 1);
        assert_eq!(watch.observe(t1, &[], true, true).state.ending_in, Some(30));
        // Back, under a new process id (Teams restarts its media helper), for more than a moment.
        let back = [user(10, "Teams")];
        let resumed = run(&mut watch, t1 + Duration::from_secs(5), NOTICE_AFTER.as_secs(), &back, true);
        assert_eq!(resumed.state, CallState { app: Some("Teams".into()), ending_in: None });
        // And that call ending later counts down again, without another minute of holding.
        let t2 = t1 + Duration::from_secs(20);
        assert_eq!(watch.observe(t2, &[], true, true).state.ending_in, Some(30));
    }

    #[test]
    fn keep_recording_cancels_the_countdown_until_the_next_call_ends() {
        let mut watch = CallWatch::new();
        let t0 = Instant::now();
        let zoom = [user(7, "Zoom")];
        run(&mut watch, t0, CALL_AFTER.as_secs(), &zoom, true);
        let t1 = t0 + Duration::from_secs(CALL_AFTER.as_secs() + 1);
        assert!(watch.observe(t1, &[], true, true).state.ending_in.is_some());
        watch.keep();
        let quiet = run(&mut watch, t1 + Duration::from_secs(1), GRACE.as_secs() + 10, &[], true);
        assert_eq!(quiet, Tick::default(), "no stop, nothing shown");
        // The next call is a call of its own, a minute to count and thirty seconds to end.
        let t2 = t1 + Duration::from_secs(100);
        run(&mut watch, t2, CALL_AFTER.as_secs(), &[user(8, "Zoom")], true);
        assert_eq!(watch.observe(t2 + Duration::from_secs(61), &[], true, true).state.ending_in, Some(30));
    }

    #[test]
    fn without_a_recording_or_with_the_setting_off_a_call_ending_shows_nothing() {
        let t0 = Instant::now();
        let zoom = [user(7, "Zoom")];
        let mut idle = CallWatch::new();
        run(&mut idle, t0, CALL_AFTER.as_secs(), &zoom, false);
        assert_eq!(idle.observe(t0 + Duration::from_secs(61), &[], false, true), Tick::default());

        let mut off = CallWatch::new();
        run(&mut off, t0, CALL_AFTER.as_secs(), &zoom, true);
        assert_eq!(off.observe(t0 + Duration::from_secs(61), &[], true, false), Tick::default());
    }

    #[test]
    fn two_call_programs_end_the_recording_only_when_both_are_gone() {
        let mut watch = CallWatch::new();
        let t0 = Instant::now();
        watch.observe(t0, &[user(7, "Zoom")], true, true);
        let both = [user(7, "Zoom"), user(9, "Teams")];
        let on = run(&mut watch, t0 + Duration::from_secs(1), CALL_AFTER.as_secs(), &both, true);
        assert_eq!(on.state.app.as_deref(), Some("Zoom"), "the first to open the microphone is named");
        let t1 = t0 + Duration::from_secs(62);
        let one_left = watch.observe(t1, &[user(9, "Teams")], true, true);
        assert_eq!(one_left.state, CallState { app: Some("Teams".into()), ending_in: None });
        assert_eq!(watch.observe(t1 + Duration::from_secs(1), &[], true, true).state.ending_in, Some(30));
    }

    #[test]
    fn programs_are_known_by_their_everyday_names_and_the_systems_own_are_not_calls() {
        assert_eq!(program_name("us.zoom.xos").as_deref(), Some("Zoom"));
        assert_eq!(program_name("Zoom.exe").as_deref(), Some("Zoom"));
        assert_eq!(program_name("com.microsoft.teams2").as_deref(), Some("Teams"));
        assert_eq!(program_name("ms-teams.exe").as_deref(), Some("Teams"));
        assert_eq!(program_name("com.tencent.meeting").as_deref(), Some("Tencent Meeting"));
        assert_eq!(program_name("wemeetapp.exe").as_deref(), Some("Tencent Meeting"));
        assert_eq!(program_name("com.google.Chrome.helper").as_deref(), Some("Chrome"));
        assert_eq!(program_name("com.apple.WebKit.GPU").as_deref(), Some("Safari"));
        assert_eq!(program_name("com.apple.FaceTime").as_deref(), Some("FaceTime"));
        assert_eq!(program_name("com.apple.CoreSpeech"), None);
        assert_eq!(program_name("svchost.exe"), None);
        assert_eq!(program_name("com.example.Whisperer").as_deref(), Some("Whisperer"));
        assert_eq!(program_name("C:\\Apps\\Voicechat.exe").as_deref(), Some("Voicechat"));
        assert_eq!(program_name("sox").as_deref(), Some("Sox"));
    }
}

#[cfg(test)]
mod live {
    /// Prints who holds the microphone, once a second for ten seconds. Open a call, or any
    /// program that listens, meanwhile.
    #[test]
    #[ignore]
    fn live_microphone_users() {
        for _ in 0..10 {
            println!("{:?}", super::microphone_users());
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
}
