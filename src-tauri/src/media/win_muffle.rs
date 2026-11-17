//! Windows "next room" engine: per-process loopback capture of each muffled
//! app (`AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`), mixed, filtered by
//! [`super::dsp::Muffler`] and played on the default output by us.
//!
//! Measured on Windows 11 (see the `live` tests): process loopback hears an
//! app *after* its mixer session volume, linearly (session 0.1 → capture
//! ≈0.1, mute or 0 → silence). So the original cannot simply be muted.
//! Instead its session is lowered to [`ATTENUATION`] (−40 dB) and the
//! capture is amplified back by `1 / ATTENUATION`: what is left of the dry
//! original sits 40 dB under the muffled copy, which hides it.
//!
//! Amplifying by 100 is only safe while the session really is attenuated,
//! so every packet is checked: audio louder than the attenuated level could
//! be (a new stream that started at full volume) blocks that app's copy for
//! a moment instead of being blasted out a hundred times louder.

use super::dsp::Muffler;
use std::collections::VecDeque;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{E_FAIL, HANDLE};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0};
use windows::Win32::System::Com::{BLOB, CLSCTX_ALL, CoCreateInstance, IAgileObject};
use windows::Win32::System::Variant::VT_BLOB;
use windows_core::{HRESULT, IUnknown, Interface, implement};

pub(crate) const RATE: u32 = 48_000;
const CHANNELS: usize = 2;
/// Share of its volume a muffled app's own session keeps (−40 dB).
pub(crate) const ATTENUATION: f32 = 0.01;
/// Captured peaks above `ATTENUATION * GUARD_FACTOR` cannot come from an
/// attenuated session (app audio is at most ~full scale).
const GUARD_FACTOR: f32 = 1.6;
const GUARD_HOLD: Duration = Duration::from_millis(300);
/// 20 ms shared-mode buffers: low latency without starving on a busy box.
const BUFFER_HNS: i64 = 200_000;
/// Audio queued per app before it is mixed, to absorb capture jitter.
const PRIME_FRAMES: usize = RATE as usize * 15 / 1000;
/// Queues longer than this have drifted; they are cut back to `PRIME_FRAMES`.
const MAX_QUEUE_FRAMES: usize = RATE as usize * 80 / 1000;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;

fn float_format() -> WAVEFORMATEX {
    WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_IEEE_FLOAT,
        nChannels: CHANNELS as u16,
        nSamplesPerSec: RATE,
        nAvgBytesPerSec: RATE * (CHANNELS as u32) * 4,
        nBlockAlign: (CHANNELS * 4) as u16,
        wBitsPerSample: 32,
        cbSize: 0,
    }
}

#[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
struct Activated(mpsc::Sender<()>);

impl windows::Win32::System::Com::IAgileObject_Impl for Activated_Impl {}

impl IActivateAudioInterfaceCompletionHandler_Impl for Activated_Impl {
    fn ActivateCompleted(&self, _operation: windows_core::Ref<IActivateAudioInterfaceAsyncOperation>) -> windows_core::Result<()> {
        let _ = self.0.send(());
        Ok(())
    }
}

/// An `IAudioClient` that hears only `pid` and its child processes.
fn process_loopback_client(pid: u32) -> windows_core::Result<IAudioClient> {
    let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    // ManuallyDrop: PROPVARIANT's Drop runs PropVariantClear, which would
    // CoTaskMemFree the blob pointer, and the blob lives on our stack.
    let variant = ManuallyDrop::new(PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 {
                    blob: BLOB {
                        cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                        pBlobData: (&mut params as *mut AUDIOCLIENT_ACTIVATION_PARAMS).cast(),
                    },
                },
            }),
        },
    });
    let (done, completed) = mpsc::channel();
    let handler: IActivateAudioInterfaceCompletionHandler = Activated(done).into();
    let operation = unsafe {
        ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, &IAudioClient::IID, Some(&*variant as *const PROPVARIANT), &handler)?
    };
    completed.recv_timeout(Duration::from_secs(2)).map_err(|_| windows_core::Error::from(E_FAIL))?;
    let mut result = HRESULT(0);
    let mut activated: Option<IUnknown> = None;
    unsafe { operation.GetActivateResult(&mut result, &mut activated)? };
    result.ok()?;
    activated.ok_or_else(|| windows_core::Error::from(E_FAIL))?.cast()
}

/// Auto-reset event owned by this engine.
struct Event(HANDLE);

impl Event {
    fn new() -> Option<Self> {
        let raw = unsafe { windows_sys::Win32::System::Threading::CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
        (!raw.is_null()).then(|| Self(HANDLE(raw)))
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0.0) };
    }
}

struct Capture {
    client: IAudioClient,
    service: IAudioCaptureClient,
    event: Event,
    queue: VecDeque<f32>,
    primed: bool,
    blocked_until: Option<Instant>,
}

fn open_capture(pid: u32) -> windows_core::Result<Capture> {
    let client = process_loopback_client(pid)?;
    let format = float_format();
    unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
            BUFFER_HNS,
            0,
            &format,
            None,
        )?;
        let event = Event::new().ok_or_else(|| windows_core::Error::from(E_FAIL))?;
        client.SetEventHandle(event.0)?;
        let service = client.GetService::<IAudioCaptureClient>()?;
        Ok(Capture {
            client,
            service,
            event,
            queue: VecDeque::with_capacity(MAX_QUEUE_FRAMES * CHANNELS),
            primed: false,
            blocked_until: None,
        })
    }
}

struct Render {
    client: IAudioClient,
    service: IAudioRenderClient,
    event: Event,
    frames: u32,
}

fn open_render() -> windows_core::Result<Render> {
    let format = float_format();
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            BUFFER_HNS,
            0,
            &format,
            None,
        )?;
        let event = Event::new().ok_or_else(|| windows_core::Error::from(E_FAIL))?;
        client.SetEventHandle(event.0)?;
        let frames = client.GetBufferSize()?;
        let service = client.GetService::<IAudioRenderClient>()?;
        Ok(Render { client, service, event, frames })
    }
}

/// Lock-free controls and meters shared with the audio thread.
struct Shared {
    stop: AtomicBool,
    amount: AtomicU32,
    amount_ramp_ms: AtomicU32,
    gain: AtomicU32,
    gain_ramp_ms: AtomicU32,
    generation: AtomicU32,
    /// Set once captured audio has been mixed into our output.
    playing: AtomicBool,
    /// Per app: RMS of the latest packet (f32 bits) and packet count.
    levels: Vec<AtomicU32>,
    packets: Vec<AtomicU64>,
    /// Times the overload guard fired, across apps.
    overloads: AtomicU64,
}

pub(crate) struct Engine {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    /// Per requested pid: whether its loopback capture started.
    pub started: Vec<bool>,
}

impl Engine {
    /// Captures each process tree and plays the filtered mix. Starts open
    /// and silent (gain 0); raise the gain once the originals are lowered.
    /// `guard` is the attenuation the originals will have, `None` disables
    /// the overload guard (measurements only). `None` result when the
    /// output or every capture failed.
    pub fn start(pids: Vec<u32>, guard: Option<f32>) -> Option<Engine> {
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            amount: AtomicU32::new(0f32.to_bits()),
            amount_ramp_ms: AtomicU32::new(0),
            gain: AtomicU32::new(0f32.to_bits()),
            gain_ramp_ms: AtomicU32::new(0),
            generation: AtomicU32::new(0),
            playing: AtomicBool::new(false),
            levels: pids.iter().map(|_| AtomicU32::new(0)).collect(),
            packets: pids.iter().map(|_| AtomicU64::new(0)).collect(),
            overloads: AtomicU64::new(0),
        });
        let (ready, started) = mpsc::channel();
        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("loquara-muffle".into())
            .spawn(move || run(pids, guard, thread_shared, ready))
            .ok()?;
        match started.recv_timeout(Duration::from_secs(4)) {
            Ok(Ok(started)) => Some(Engine { shared, thread: Some(thread), started }),
            Ok(Err(error)) => {
                eprintln!("Loquara media: muffle unavailable: {error}");
                let _ = thread.join();
                None
            }
            Err(_) => {
                shared.stop.store(true, Ordering::SeqCst);
                None
            }
        }
    }

    /// Filter position: 0 open, 1 fully muffled.
    pub fn set_amount(&self, amount: f32, ramp: Duration) {
        self.shared.amount.store(amount.to_bits(), Ordering::SeqCst);
        self.shared.amount_ramp_ms.store(ramp.as_millis() as u32, Ordering::SeqCst);
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
    }

    /// Linear gain applied to the captured mix before filtering.
    pub fn set_gain(&self, gain: f32, ramp: Duration) {
        self.shared.gain.store(gain.to_bits(), Ordering::SeqCst);
        self.shared.gain_ramp_ms.store(ramp.as_millis() as u32, Ordering::SeqCst);
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
    }

    /// Waits until every started capture has queued enough to be mixed.
    pub fn wait_playing(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while !self.shared.playing.load(Ordering::SeqCst) {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        true
    }

    #[allow(dead_code)]
    pub fn level(&self, app: usize) -> f32 {
        f32::from_bits(self.shared.levels[app].load(Ordering::SeqCst))
    }

    #[allow(dead_code)]
    pub fn packets(&self, app: usize) -> u64 {
        self.shared.packets[app].load(Ordering::SeqCst)
    }

    #[allow(dead_code)]
    pub fn overloads(&self) -> u64 {
        self.shared.overloads.load(Ordering::SeqCst)
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Linear per-frame gain glide.
struct Glide {
    value: f32,
    target: f32,
    step: f32,
    left: u32,
}

impl Glide {
    fn set(&mut self, target: f32, frames: u32) {
        self.target = target;
        if frames == 0 {
            self.value = target;
            self.left = 0;
        } else {
            self.step = (target - self.value) / frames as f32;
            self.left = frames;
        }
    }

    fn next(&mut self) -> f32 {
        if self.left > 0 {
            self.left -= 1;
            self.value = if self.left == 0 { self.target } else { self.value + self.step };
        }
        self.value
    }
}

fn run(pids: Vec<u32>, guard: Option<f32>, shared: Arc<Shared>, ready: mpsc::Sender<Result<Vec<bool>, String>>) {
    let Some(_com) = super::win::init_thread() else {
        let _ = ready.send(Err("COM init failed".into()));
        return;
    };
    unsafe {
        use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL};
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
    }
    let render = match open_render() {
        Ok(render) => render,
        Err(error) => {
            let _ = ready.send(Err(format!("output: {error}")));
            return;
        }
    };
    let mut captures: Vec<Option<Capture>> = pids
        .iter()
        .map(|pid| match open_capture(*pid) {
            Ok(capture) => Some(capture),
            Err(error) => {
                eprintln!("Loquara media: cannot capture process {pid}: {error}");
                None
            }
        })
        .collect();
    let started: Vec<bool> = captures.iter().map(Option::is_some).collect();
    if !started.iter().any(|ok| *ok) {
        let _ = ready.send(Err("no app could be captured".into()));
        return;
    }
    let begun = unsafe { render.client.Start().and_then(|()| captures.iter().flatten().try_for_each(|capture| capture.client.Start())) };
    if let Err(error) = begun {
        let _ = ready.send(Err(format!("start: {error}")));
        return;
    }
    let _ = ready.send(Ok(started));

    let guard = guard.map(|attenuation| attenuation * GUARD_FACTOR);
    let mut muffler = Muffler::new(RATE, CHANNELS);
    let mut gain = Glide { value: 0.0, target: 0.0, step: 0.0, left: 0 };
    let mut generation = 0;
    let mut mix = vec![0f32; render.frames as usize * CHANNELS];
    let mut events: Vec<*mut core::ffi::c_void> = vec![render.event.0.0];
    events.extend(captures.iter().flatten().map(|capture| capture.event.0.0));
    while !shared.stop.load(Ordering::SeqCst) {
        unsafe {
            windows_sys::Win32::System::Threading::WaitForMultipleObjects(events.len() as u32, events.as_ptr(), 0, 20);
        }
        let current = shared.generation.load(Ordering::SeqCst);
        if current != generation {
            generation = current;
            let ms = |value: &AtomicU32| u64::from(value.load(Ordering::SeqCst));
            muffler.set_target(f32::from_bits(shared.amount.load(Ordering::SeqCst)), Duration::from_millis(ms(&shared.amount_ramp_ms)));
            let frames = (ms(&shared.gain_ramp_ms) * u64::from(RATE) / 1000) as u32;
            let was_armed = gain.target > 1.0;
            gain.set(f32::from_bits(shared.gain.load(Ordering::SeqCst)), frames);
            if !was_armed && gain.target > 1.0 {
                // Amplification starts now: drop anything queued from before
                // the originals were lowered.
                for capture in captures.iter_mut().flatten() {
                    capture.queue.clear();
                    capture.primed = false;
                    capture.blocked_until = None;
                }
            }
        }
        // The guard only matters while the copy is amplified.
        let armed_guard = guard.filter(|_| gain.target > 1.0);
        for (index, capture) in captures.iter_mut().enumerate() {
            if let Some(capture) = capture {
                drain(capture, &shared, index, armed_guard);
            }
        }
        if !write_output(&render, &mut captures, &mut muffler, &mut gain, &mut mix, &shared) {
            break;
        }
    }
    unsafe {
        let _ = render.client.Stop();
        let _ = render.client.Reset();
        for capture in captures.iter().flatten() {
            let _ = capture.client.Stop();
        }
    }
}

/// Moves every pending packet of one app into its queue.
fn drain(capture: &mut Capture, shared: &Shared, index: usize, guard: Option<f32>) {
    let now = Instant::now();
    if capture.blocked_until.is_some_and(|until| now >= until) {
        capture.blocked_until = None;
    }
    loop {
        let pending = unsafe { capture.service.GetNextPacketSize() }.unwrap_or(0);
        if pending == 0 {
            break;
        }
        let mut data = std::ptr::null_mut();
        let mut frames = 0u32;
        let mut flags = 0u32;
        if unsafe { capture.service.GetBuffer(&mut data, &mut frames, &mut flags, None, None) }.is_err() {
            break;
        }
        let samples = frames as usize * CHANNELS;
        let silent = flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null();
        let packet: &[f32] = if silent { &[] } else { unsafe { std::slice::from_raw_parts(data as *const f32, samples) } };
        let peak = packet.iter().fold(0f32, |peak, sample| peak.max(sample.abs()));
        let energy: f32 = packet.iter().map(|sample| sample * sample).sum();
        if guard.is_some_and(|limit| peak > limit) {
            // Louder than an attenuated session can be: never amplify it.
            capture.blocked_until = Some(now + GUARD_HOLD);
            capture.queue.clear();
            capture.primed = false;
            shared.overloads.fetch_add(1, Ordering::SeqCst);
        } else if capture.blocked_until.is_none() {
            if silent {
                capture.queue.extend(std::iter::repeat_n(0.0, samples));
            } else {
                capture.queue.extend(packet.iter().copied());
            }
        }
        unsafe {
            let _ = capture.service.ReleaseBuffer(frames);
        }
        let rms = if packet.is_empty() { 0.0 } else { (energy / packet.len() as f32).sqrt() };
        shared.levels[index].store(rms.to_bits(), Ordering::SeqCst);
        shared.packets[index].fetch_add(1, Ordering::SeqCst);
    }
    let queued = capture.queue.len() / CHANNELS;
    if queued > MAX_QUEUE_FRAMES {
        capture.queue.drain(..(queued - PRIME_FRAMES) * CHANNELS);
    }
    if !capture.primed && capture.queue.len() / CHANNELS >= PRIME_FRAMES {
        capture.primed = true;
    }
}

/// Fills whatever room the output buffer has. False when the output died
/// (device removed); the engine then stops and the session restores.
fn write_output(
    render: &Render,
    captures: &mut [Option<Capture>],
    muffler: &mut Muffler,
    gain: &mut Glide,
    mix: &mut [f32],
    shared: &Shared,
) -> bool {
    let Ok(padding) = (unsafe { render.client.GetCurrentPadding() }) else { return false };
    let free = render.frames.saturating_sub(padding) as usize;
    if free == 0 {
        return true;
    }
    let mix = &mut mix[..free * CHANNELS];
    mix.fill(0.0);
    let mut mixed = false;
    for capture in captures.iter_mut().flatten() {
        if !capture.primed {
            continue;
        }
        let take = mix.len().min(capture.queue.len());
        for (slot, sample) in mix.iter_mut().zip(capture.queue.drain(..take)) {
            *slot += sample;
        }
        mixed = true;
        if capture.queue.is_empty() {
            // Ran dry: rebuild the cushion before mixing it again.
            capture.primed = false;
        }
    }
    for frame in mix.chunks_mut(CHANNELS) {
        let gain = gain.next();
        for sample in frame {
            *sample *= gain;
        }
    }
    muffler.process(mix);
    let Ok(buffer) = (unsafe { render.service.GetBuffer(free as u32) }) else { return false };
    unsafe {
        std::ptr::copy_nonoverlapping(mix.as_ptr(), buffer as *mut f32, mix.len());
        if render.service.ReleaseBuffer(free as u32, 0).is_err() {
            return false;
        }
    }
    if mixed {
        shared.playing.store(true, Ordering::SeqCst);
    }
    true
}

/// Records what one process tree plays, without playing anything back.
/// Used by the live tests to listen to our own output.
#[cfg(test)]
pub(crate) fn record(pid: u32, duration: Duration) -> Vec<f32> {
    let mut capture = open_capture(pid).expect("probe capture");
    unsafe { capture.client.Start().unwrap() };
    let shared = Shared {
        stop: AtomicBool::new(false),
        amount: AtomicU32::new(0),
        amount_ramp_ms: AtomicU32::new(0),
        gain: AtomicU32::new(0),
        gain_ramp_ms: AtomicU32::new(0),
        generation: AtomicU32::new(0),
        playing: AtomicBool::new(false),
        levels: vec![AtomicU32::new(0)],
        packets: vec![AtomicU64::new(0)],
        overloads: AtomicU64::new(0),
    };
    let deadline = Instant::now() + duration;
    let mut recorded = Vec::new();
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
        drain(&mut capture, &shared, 0, None);
        recorded.extend(capture.queue.drain(..));
    }
    unsafe {
        let _ = capture.client.Stop();
    }
    recorded
}
