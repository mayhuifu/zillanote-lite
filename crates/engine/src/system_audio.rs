//! The computer's own sound: what the other side of a call says.
//!
//! macOS hands it over through a Core Audio process tap (14.2 and later). The first use
//! makes macOS ask for the "System Audio Recording" permission; without it the tap still
//! runs and delivers silence, so a refusal costs the far side of the call and nothing else.

use std::sync::mpsc;

/// What the tap delivers: interleaved blocks at `rate` with `channels` channels.
pub struct SystemAudioStream {
    pub rate: u32,
    pub channels: usize,
    pub blocks: mpsc::Receiver<Vec<f32>>,
}

/// Capturing runs for as long as this is alive. It is not `Send`: keep it on the thread
/// that started it.
pub struct SystemAudio {
    #[cfg(target_os = "macos")]
    _tap: macos::Tap,
}

impl SystemAudio {
    #[cfg(target_os = "macos")]
    pub fn start() -> Result<(Self, SystemAudioStream), String> {
        let (tap, stream) = macos::Tap::start()?;
        Ok((Self { _tap: tap }, stream))
    }

    #[cfg(not(target_os = "macos"))]
    pub fn start() -> Result<(Self, SystemAudioStream), String> {
        Err("Recording the computer's sound is only built for macOS so far.".to_string())
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::{CStr, c_void};
    use std::ptr::NonNull;
    use std::sync::mpsc;

    use objc2::AllocAnyThread;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_core_audio::{
        AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart,
        AudioDeviceStop, AudioHardwareCreateAggregateDevice, AudioHardwareDestroyAggregateDevice,
        AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress, CATapDescription,
        kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceNameKey, kAudioAggregateDeviceTapAutoStartKey,
        kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey, kAudioObjectPropertyElementMain,
        kAudioObjectPropertyScopeGlobal, kAudioSubTapUIDKey, kAudioTapPropertyFormat,
    };
    use objc2_core_audio_types::{
        AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp, kAudioFormatFlagIsFloat,
        kAudioFormatFlagIsNonInterleaved, kAudioFormatLinearPCM,
    };
    use objc2_core_foundation::CFDictionary;
    use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSUUID};

    use super::SystemAudioStream;

    const TOO_OLD: &str = "Recording the computer's sound needs macOS 14.2 or later.";

    // Looked up when first used instead of linked, so the app still starts on a macOS that
    // does not have taps yet.
    type CreateProcessTap = unsafe extern "C-unwind" fn(*const CATapDescription, *mut AudioObjectID) -> i32;
    type DestroyProcessTap = unsafe extern "C-unwind" fn(AudioObjectID) -> i32;

    fn symbol(name: &CStr) -> Option<NonNull<c_void>> {
        NonNull::new(unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) })
    }

    pub struct Tap {
        tap_id: AudioObjectID,
        device_id: AudioObjectID,
        proc_id: AudioDeviceIOProcID,
        running: bool,
        /// Borrowed by the IO proc; freed only after the device has stopped calling it.
        sender: *mut mpsc::Sender<Vec<f32>>,
    }

    impl Tap {
        pub fn start() -> Result<(Self, SystemAudioStream), String> {
            let create = symbol(c"AudioHardwareCreateProcessTap").ok_or(TOO_OLD)?;
            let create: CreateProcessTap = unsafe { std::mem::transmute(create.as_ptr()) };
            if AnyClass::get(c"CATapDescription").is_none() {
                return Err(TOO_OLD.to_string());
            }

            // Everything every process plays, mixed down to one channel.
            let description = unsafe {
                CATapDescription::initMonoGlobalTapButExcludeProcesses(CATapDescription::alloc(), &NSArray::new())
            };
            unsafe {
                description.setName(&NSString::from_str("ZillaNote"));
                description.setPrivate(true);
            }
            let tap_uid = unsafe { description.UUID().UUIDString() };

            let mut tap_id: AudioObjectID = 0;
            check(unsafe { create(&*description, &mut tap_id) }, "create the tap")?;

            let (sender, blocks) = mpsc::channel();
            // From here on, dropping `tap` undoes whatever was set up so far.
            let mut tap = Self {
                tap_id,
                device_id: 0,
                proc_id: None,
                running: false,
                sender: Box::into_raw(Box::new(sender)),
            };

            let format = tap.format()?;
            let is_float = format.mFormatID == kAudioFormatLinearPCM
                && format.mFormatFlags & kAudioFormatFlagIsFloat != 0
                && format.mBitsPerChannel == 32;
            if !is_float {
                return Err("The computer's sound comes in a format ZillaNote does not read.".to_string());
            }
            // Separate buffers per channel: the first one is read, so it counts as one.
            let channels = match format.mFormatFlags & kAudioFormatFlagIsNonInterleaved {
                0 => format.mChannelsPerFrame.max(1) as usize,
                _ => 1,
            };

            let device = aggregate_device_description(&tap_uid);
            let device: &CFDictionary = unsafe { &*Retained::as_ptr(&device).cast() };
            check(
                unsafe { AudioHardwareCreateAggregateDevice(device, NonNull::from(&mut tap.device_id)) },
                "create the capture device",
            )?;
            check(
                unsafe {
                    AudioDeviceCreateIOProcID(
                        tap.device_id,
                        Some(io_proc),
                        tap.sender.cast(),
                        NonNull::from(&mut tap.proc_id),
                    )
                },
                "attach to the capture device",
            )?;
            check(unsafe { AudioDeviceStart(tap.device_id, tap.proc_id) }, "start the capture device")?;
            tap.running = true;

            let stream = SystemAudioStream {
                rate: format.mSampleRate as u32,
                channels,
                blocks,
            };
            Ok((tap, stream))
        }

        fn format(&self) -> Result<AudioStreamBasicDescription, String> {
            let address = AudioObjectPropertyAddress {
                mSelector: kAudioTapPropertyFormat,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain,
            };
            let mut format = std::mem::MaybeUninit::<AudioStreamBasicDescription>::zeroed();
            let mut size = size_of::<AudioStreamBasicDescription>() as u32;
            check(
                unsafe {
                    AudioObjectGetPropertyData(
                        self.tap_id,
                        NonNull::from(&address),
                        0,
                        std::ptr::null(),
                        NonNull::from(&mut size),
                        NonNull::new_unchecked(format.as_mut_ptr().cast()),
                    )
                },
                "read the tap's format",
            )?;
            // Plain numbers, and zeroed beforehand: valid whatever was written.
            Ok(unsafe { format.assume_init() })
        }
    }

    impl Drop for Tap {
        fn drop(&mut self) {
            unsafe {
                if self.running {
                    AudioDeviceStop(self.device_id, self.proc_id);
                }
                if self.proc_id.is_some() {
                    AudioDeviceDestroyIOProcID(self.device_id, self.proc_id);
                }
                if self.device_id != 0 {
                    AudioHardwareDestroyAggregateDevice(self.device_id);
                }
                if let Some(destroy) = symbol(c"AudioHardwareDestroyProcessTap") {
                    let destroy: DestroyProcessTap = std::mem::transmute(destroy.as_ptr());
                    destroy(self.tap_id);
                }
                drop(Box::from_raw(self.sender));
            }
        }
    }

    /// A device nobody else sees, whose only input is the tap.
    fn aggregate_device_description(tap_uid: &NSString) -> Retained<NSDictionary<NSString, AnyObject>> {
        let key = |key: &CStr| NSString::from_str(key.to_str().expect("an ASCII key"));

        let tap = NSDictionary::<NSString, AnyObject>::from_slices(&[&*key(kAudioSubTapUIDKey)], &[tap_uid]);
        let taps = NSArray::from_retained_slice(&[tap]);
        let name = NSString::from_str("ZillaNote system audio");
        let uid = NSUUID::new().UUIDString();
        let yes = NSNumber::numberWithBool(true);
        let no = NSNumber::numberWithBool(false);

        NSDictionary::from_slices(
            &[
                &*key(kAudioAggregateDeviceNameKey),
                &*key(kAudioAggregateDeviceUIDKey),
                &*key(kAudioAggregateDeviceIsPrivateKey),
                &*key(kAudioAggregateDeviceTapAutoStartKey),
                &*key(kAudioAggregateDeviceTapListKey),
            ],
            &[&*name, &*uid, &*yes, &*no, &*taps],
        )
    }

    /// Runs on Core Audio's real-time thread.
    unsafe extern "C-unwind" fn io_proc(
        _device: AudioObjectID,
        _now: NonNull<AudioTimeStamp>,
        input: NonNull<AudioBufferList>,
        _input_time: NonNull<AudioTimeStamp>,
        _output: NonNull<AudioBufferList>,
        _output_time: NonNull<AudioTimeStamp>,
        client: *mut c_void,
    ) -> i32 {
        let list = unsafe { input.as_ref() };
        if client.is_null() || list.mNumberBuffers == 0 {
            return 0;
        }
        let buffer = &list.mBuffers[0];
        let data = buffer.mData as *const f32;
        if data.is_null() || !data.is_aligned() || buffer.mDataByteSize < 4 {
            return 0;
        }

        let samples = unsafe { std::slice::from_raw_parts(data, buffer.mDataByteSize as usize / 4) };
        let sender = unsafe { &*(client as *const mpsc::Sender<Vec<f32>>) };
        let _ = sender.send(samples.to_vec());
        0
    }

    fn check(status: i32, what: &str) -> Result<(), String> {
        match status {
            0 => Ok(()),
            status => Err(format!("Could not {what} for the computer's sound (Core Audio error {status}).")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plays a system sound and expects to hear it through the tap. Needs macOS 14.2 and the
    /// "System Audio Recording" permission for whatever runs the test (the terminal):
    ///
    /// cargo test -p engine live_system_audio -- --ignored --nocapture
    #[test]
    #[ignore = "plays a sound, and needs the System Audio Recording permission"]
    fn live_system_audio_hears_what_the_computer_plays() {
        let (capture, stream) = SystemAudio::start().expect("the tap");
        println!("{} Hz, {} channel(s)", stream.rate, stream.channels);

        let player = std::process::Command::new("afplay")
            .arg("/System/Library/Sounds/Glass.aiff")
            .status()
            .expect("afplay");
        assert!(player.success());
        std::thread::sleep(std::time::Duration::from_millis(300));
        drop(capture);

        let samples = stream.blocks.try_iter().flatten().collect::<Vec<f32>>();
        let seconds = samples.len() as f64 / (stream.rate as f64 * stream.channels as f64);
        let peak = samples.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        println!("{seconds:.2} s captured, peak {peak:.4}");

        assert!(seconds > 1.0, "only {seconds:.2} s arrived");
        assert!(peak > 0.001, "silence: is the permission granted, and the volume up?");
    }
}
