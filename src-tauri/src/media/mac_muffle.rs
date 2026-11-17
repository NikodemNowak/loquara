//! macOS 14.2+ "next room": a Core Audio process tap on the muffled apps
//! (`CATapMutedWhenTapped`, so they fall silent exactly while we read them)
//! inside a private aggregate device whose main sub-device is the default
//! output. One IOProc reads the tap, runs [`super::dsp::Muffler`] and writes
//! the result to the output. Destroying the tap (also by the system when we
//! crash) unmutes the apps, so there is nothing to recover.
//!
//! Needs `NSAudioCaptureUsageDescription` in Info.plist; macOS asks the user
//! once. When access is denied the tap delivers only zeros, which is
//! detected and answered by falling back to ducking.

use super::dsp::Muffler;
use super::{Muffle, Policy, Stop, Target};
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_core_audio::{
    AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop,
    AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap, AudioHardwareDestroyAggregateDevice,
    AudioHardwareDestroyProcessTap, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
    AudioObjectPropertyAddress, CATapDescription, CATapMuteBehavior, kAudioAggregateDeviceIsPrivateKey,
    kAudioAggregateDeviceMainSubDeviceKey, kAudioAggregateDeviceNameKey, kAudioAggregateDeviceSubDeviceListKey,
    kAudioAggregateDeviceTapAutoStartKey, kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey,
    kAudioDevicePropertyDeviceUID, kAudioDevicePropertyNominalSampleRate, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioHardwarePropertyProcessObjectList, kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal,
    kAudioObjectSystemObject, kAudioProcessPropertyIsRunningOutput, kAudioProcessPropertyPID, kAudioSubDeviceUIDKey,
    kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey,
};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList, AudioTimeStamp};
use objc2_core_foundation::{
    CFArray, CFDictionary, CFMutableDictionary, CFRetained, CFString, kCFAllocatorDefault, kCFTypeArrayCallBacks,
    kCFTypeDictionaryKeyCallBacks, kCFTypeDictionaryValueCallBacks,
};
use objc2_foundation::{NSArray, NSNumber, NSString};
use std::ffi::{CStr, c_void};
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const MUFFLE_RAMP: Duration = Duration::from_millis(300);
/// Largest IO cycle we prepare for without allocating in the callback.
const MAX_FRAMES: usize = 16_384;

fn address(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress { mSelector: selector, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain }
}

fn scalar<T: Copy + Default>(object: AudioObjectID, selector: u32) -> Option<T> {
    let address = address(selector);
    let mut value = T::default();
    let mut size = std::mem::size_of::<T>() as u32;
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

fn process_objects() -> Option<Vec<AudioObjectID>> {
    let address = address(kAudioHardwarePropertyProcessObjectList);
    let mut size = 0u32;
    let status = unsafe {
        AudioObjectGetPropertyDataSize(kAudioObjectSystemObject as AudioObjectID, NonNull::from(&address), 0, std::ptr::null(), NonNull::from(&mut size))
    };
    if status != 0 {
        return None;
    }
    let mut objects = vec![0 as AudioObjectID; size as usize / std::mem::size_of::<AudioObjectID>()];
    if objects.is_empty() {
        return Some(objects);
    }
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(objects.as_mut_ptr())?.cast(),
        )
    };
    if status != 0 {
        return None;
    }
    objects.truncate(size as usize / std::mem::size_of::<AudioObjectID>());
    Some(objects)
}

fn device_uid(device: AudioObjectID) -> Option<Retained<NSString>> {
    let address = address(kAudioDevicePropertyDeviceUID);
    let mut text: *mut c_void = std::ptr::null_mut();
    let mut size = std::mem::size_of::<*mut c_void>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(device, NonNull::from(&address), 0, std::ptr::null(), NonNull::from(&mut size), NonNull::from(&mut text).cast())
    };
    if status != 0 || text.is_null() {
        return None;
    }
    // The property hands over a +1 CFString; NSString is toll-free bridged.
    unsafe { Retained::from_raw(text.cast::<NSString>()) }
}

fn cf_key(key: &'static CStr) -> CFRetained<CFString> {
    CFString::from_str(key.to_str().unwrap_or_default())
}

fn cf_dictionary(entries: &[(&'static CStr, *const c_void)]) -> CFRetained<CFMutableDictionary> {
    let dictionary = unsafe {
        CFMutableDictionary::new(kCFAllocatorDefault, entries.len() as _, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks)
    }
    .expect("CFDictionary");
    for (key, value) in entries {
        let key = cf_key(key);
        unsafe { CFMutableDictionary::set_value(Some(dictionary.as_ref()), &*key as *const CFString as *const c_void, *value) };
    }
    dictionary
}

fn cf_array(items: &[*const c_void]) -> CFRetained<CFArray> {
    unsafe { CFArray::new(kCFAllocatorDefault, items.as_ptr() as *mut *const c_void, items.len() as _, &kCFTypeArrayCallBacks) }.expect("CFArray")
}

/// A private aggregate of the default output (main device) and our tap.
fn create_aggregate(output_uid: &NSString, tap_uid: &NSString) -> Option<AudioObjectID> {
    let yes = NSNumber::new_bool(true);
    let yes_ptr = &*yes as *const NSNumber as *const c_void;
    let tap = cf_dictionary(&[(kAudioSubTapUIDKey, tap_uid as *const NSString as *const c_void), (kAudioSubTapDriftCompensationKey, yes_ptr)]);
    let taps = cf_array(&[&*tap as *const CFMutableDictionary as *const c_void]);
    let output = cf_dictionary(&[(kAudioSubDeviceUIDKey, output_uid as *const NSString as *const c_void)]);
    let devices = cf_array(&[&*output as *const CFMutableDictionary as *const c_void]);
    let name = CFString::from_str("Loquara muffle");
    let uid = CFString::from_str(&format!("com.loquara.muffle.{}", std::process::id()));
    let description = cf_dictionary(&[
        (kAudioAggregateDeviceNameKey, &*name as *const CFString as *const c_void),
        (kAudioAggregateDeviceUIDKey, &*uid as *const CFString as *const c_void),
        (kAudioAggregateDeviceMainSubDeviceKey, output_uid as *const NSString as *const c_void),
        (kAudioAggregateDeviceSubDeviceListKey, &*devices as *const CFArray as *const c_void),
        (kAudioAggregateDeviceTapListKey, &*taps as *const CFArray as *const c_void),
        (kAudioAggregateDeviceTapAutoStartKey, yes_ptr),
        (kAudioAggregateDeviceIsPrivateKey, yes_ptr),
    ]);
    let description: &CFDictionary = &description;
    let mut device: AudioObjectID = 0;
    let status = unsafe { AudioHardwareCreateAggregateDevice(description, NonNull::from(&mut device)) };
    (status == 0).then_some(device)
}

/// Shared with the IO callback.
struct Controls {
    amount: AtomicU32,
    ramp_ms: AtomicU32,
    generation: AtomicU32,
    /// Largest input sample seen (f32 bits): tells permission problems apart.
    peak: AtomicU32,
}

/// Owned by the IO callback through its client pointer.
struct Callback {
    muffler: Muffler,
    generation: u32,
    scratch: Vec<f32>,
    controls: Arc<Controls>,
}

unsafe fn buffers<'a>(list: NonNull<AudioBufferList>) -> &'a [AudioBuffer] {
    let list = unsafe { list.as_ref() };
    unsafe { std::slice::from_raw_parts(list.mBuffers.as_ptr(), list.mNumberBuffers as usize) }
}

/// Sample `channel` (counted across all buffers) of `frame`; 0 when absent.
fn sample(buffers: &[AudioBuffer], channel: usize, frame: usize) -> f32 {
    let mut first = 0;
    for buffer in buffers {
        let channels = buffer.mNumberChannels as usize;
        if channel < first + channels {
            let frames = buffer.mDataByteSize as usize / (4 * channels.max(1));
            if frame >= frames || buffer.mData.is_null() {
                return 0.0;
            }
            return unsafe { *(buffer.mData as *const f32).add(frame * channels + channel - first) };
        }
        first += channels;
    }
    0.0
}

fn frames(buffers: &[AudioBuffer]) -> usize {
    buffers
        .iter()
        .filter(|buffer| buffer.mNumberChannels > 0)
        .map(|buffer| buffer.mDataByteSize as usize / (4 * buffer.mNumberChannels as usize))
        .min()
        .unwrap_or(0)
}

unsafe extern "C-unwind" fn io_proc(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    client: *mut c_void,
) -> i32 {
    let Some(state) = (unsafe { (client as *mut Callback).as_mut() }) else { return 0 };
    let inputs = unsafe { buffers(input) };
    let outputs = unsafe { buffers(output) };
    let count = frames(outputs).min(MAX_FRAMES);
    let input_channels: usize = inputs.iter().map(|buffer| buffer.mNumberChannels as usize).sum();
    let generation = state.controls.generation.load(Ordering::Relaxed);
    if generation != state.generation {
        state.generation = generation;
        let amount = f32::from_bits(state.controls.amount.load(Ordering::Relaxed));
        let ramp = Duration::from_millis(u64::from(state.controls.ramp_ms.load(Ordering::Relaxed)));
        state.muffler.set_target(amount, ramp);
    }
    let available = frames(inputs);
    let mut peak = 0f32;
    let scratch = &mut state.scratch[..count * 2];
    for frame in 0..count {
        let (left, right) = if frame >= available || input_channels == 0 {
            (0.0, 0.0)
        } else if input_channels == 1 {
            let mono = sample(inputs, 0, frame);
            (mono, mono)
        } else {
            (sample(inputs, 0, frame), sample(inputs, 1, frame))
        };
        peak = peak.max(left.abs()).max(right.abs());
        scratch[frame * 2] = left;
        scratch[frame * 2 + 1] = right;
    }
    state.muffler.process(scratch);
    if peak > f32::from_bits(state.controls.peak.load(Ordering::Relaxed)) {
        state.controls.peak.store(peak.to_bits(), Ordering::Relaxed);
    }
    let mut first = 0;
    for buffer in outputs {
        let channels = buffer.mNumberChannels as usize;
        if !buffer.mData.is_null() && channels > 0 {
            let samples = buffer.mDataByteSize as usize / 4;
            let data = unsafe { std::slice::from_raw_parts_mut(buffer.mData as *mut f32, samples) };
            for (index, slot) in data.iter_mut().enumerate() {
                let (frame, channel) = (index / channels, first + index % channels);
                *slot = match channel {
                    _ if frame >= count => 0.0,
                    0 => scratch[frame * 2],
                    1 => scratch[frame * 2 + 1],
                    _ => 0.0,
                };
            }
        }
        first += channels;
    }
    0
}

/// `sw_vers -productVersion` is at least `major.minor`.
pub(crate) fn version_at_least(version: &str, major: u32, minor: u32) -> bool {
    let mut parts = version.trim().split('.').map(|part| part.parse::<u32>().unwrap_or(0));
    let found = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    found >= (major, minor)
}

struct Muffled {
    tap: AudioObjectID,
    aggregate: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    controls: Arc<Controls>,
    callback: *mut Callback,
    claimed: Vec<String>,
}

// The raw pointers are only touched by Core Audio's IO thread (through the
// registered callback) and by `end`, after the device has been stopped.
unsafe impl Send for Muffled {}

impl Muffled {
    fn set_amount(&self, amount: f32, ramp: Duration) {
        self.controls.amount.store(amount.to_bits(), Ordering::SeqCst);
        self.controls.ramp_ms.store(ramp.as_millis() as u32, Ordering::SeqCst);
        self.controls.generation.fetch_add(1, Ordering::SeqCst);
    }

    fn teardown(&mut self) {
        unsafe {
            if self.proc_id.is_some() {
                AudioDeviceStop(self.aggregate, self.proc_id);
                AudioDeviceDestroyIOProcID(self.aggregate, self.proc_id);
                self.proc_id = None;
            }
            if self.aggregate != 0 {
                AudioHardwareDestroyAggregateDevice(self.aggregate);
                self.aggregate = 0;
            }
            if self.tap != 0 {
                AudioHardwareDestroyProcessTap(self.tap);
                self.tap = 0;
            }
            if !self.callback.is_null() {
                drop(Box::from_raw(self.callback));
                self.callback = std::ptr::null_mut();
            }
        }
    }
}

impl Drop for Muffled {
    fn drop(&mut self) {
        self.teardown();
    }
}

pub(crate) fn muffle_begin(policy: &Policy, stop: &mut Stop, names: impl Fn(&[i32]) -> Option<Vec<String>>) -> Option<Box<dyn Muffle>> {
    let version = super::command::run("sw_vers", &["-productVersion"], Duration::from_secs(2))?;
    if !version_at_least(&version, 14, 2) {
        return None;
    }
    let own = std::process::id() as i32;
    let playing: Vec<(AudioObjectID, i32)> = process_objects()?
        .into_iter()
        .filter(|object| scalar::<u32>(*object, kAudioProcessPropertyIsRunningOutput).unwrap_or(0) != 0)
        .filter_map(|object| Some((object, scalar::<i32>(object, kAudioProcessPropertyPID)?)))
        .filter(|(_, pid)| *pid > 0 && *pid != own)
        .collect();
    let pids: Vec<i32> = playing.iter().map(|(_, pid)| *pid).collect();
    let names = names(&pids)?;
    let chosen: Vec<(AudioObjectID, i32)> = playing
        .into_iter()
        .zip(names.iter().map(String::as_str).chain(std::iter::repeat("")))
        .filter(|(_, name)| policy.allows(name))
        .map(|(process, _)| process)
        .collect();
    if chosen.is_empty() {
        return None;
    }
    let output = scalar::<AudioObjectID>(kAudioObjectSystemObject as AudioObjectID, kAudioHardwarePropertyDefaultOutputDevice)?;
    let output_uid = device_uid(output)?;

    let numbers: Vec<Retained<NSNumber>> = chosen.iter().map(|(object, _)| NSNumber::new_u32(*object)).collect();
    let processes = NSArray::from_retained_slice(&numbers);
    let description = unsafe { CATapDescription::initStereoMixdownOfProcesses(CATapDescription::alloc(), &processes) };
    unsafe {
        description.setMuteBehavior(CATapMuteBehavior::MutedWhenTapped);
        description.setPrivate(true);
        description.setName(&NSString::from_str("Loquara muffle"));
    }
    let mut tap: AudioObjectID = 0;
    if unsafe { AudioHardwareCreateProcessTap(Some(&description), &mut tap) } != 0 {
        return None;
    }
    let tap_uid = unsafe { description.UUID().UUIDString() };
    let mut muffled = Muffled {
        tap,
        aggregate: 0,
        proc_id: None,
        controls: Arc::new(Controls { amount: AtomicU32::new(0), ramp_ms: AtomicU32::new(0), generation: AtomicU32::new(0), peak: AtomicU32::new(0) }),
        callback: std::ptr::null_mut(),
        claimed: chosen.iter().map(|(_, pid)| format!("pid:{pid}")).collect(),
    };
    muffled.aggregate = create_aggregate(&output_uid, &tap_uid)?;
    let rate = scalar::<f64>(muffled.aggregate, kAudioDevicePropertyNominalSampleRate).unwrap_or(48_000.0);
    muffled.callback = Box::into_raw(Box::new(Callback {
        muffler: Muffler::new(rate as u32, 2),
        generation: 0,
        scratch: vec![0.0; MAX_FRAMES * 2],
        controls: muffled.controls.clone(),
    }));
    let mut proc_id: AudioDeviceIOProcID = None;
    let created = unsafe { AudioDeviceCreateIOProcID(muffled.aggregate, Some(io_proc), muffled.callback.cast(), NonNull::from(&mut proc_id)) };
    if created != 0 || proc_id.is_none() {
        return None;
    }
    muffled.proc_id = proc_id;
    if unsafe { AudioDeviceStart(muffled.aggregate, muffled.proc_id) } != 0 {
        return None;
    }
    // Denied capture permission shows up as a tap of pure zeros.
    let deadline = Instant::now() + Duration::from_millis(600);
    while f32::from_bits(muffled.controls.peak.load(Ordering::SeqCst)) < 1e-6 {
        if Instant::now() >= deadline || stop.wait_for(Duration::from_millis(20)) {
            eprintln!("Loquara media: process tap delivered no audio; ducking instead");
            return None;
        }
    }
    muffled.set_amount(1.0, MUFFLE_RAMP);
    Some(Box::new(muffled))
}

impl Muffle for Muffled {
    fn claimed(&self) -> Vec<String> {
        self.claimed.clone()
    }

    fn targets(&self) -> &[Target] {
        &[]
    }

    fn end(self: Box<Self>) {
        let mut this = *self;
        this.set_amount(0.0, MUFFLE_RAMP);
        std::thread::sleep(MUFFLE_RAMP + Duration::from_millis(30));
        this.teardown();
    }
}

#[cfg(test)]
mod tests {
    use super::version_at_least;

    #[test]
    fn compares_macos_versions() {
        assert!(version_at_least("14.2", 14, 2));
        assert!(version_at_least("14.6.1\n", 14, 2));
        assert!(version_at_least("15.0", 14, 2));
        assert!(!version_at_least("14.1.2", 14, 2));
        assert!(!version_at_least("13.6", 14, 2));
        assert!(!version_at_least("", 14, 2));
    }
}
