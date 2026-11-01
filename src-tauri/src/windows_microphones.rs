//! Endpoint callbacks queue changes; registration and release stay on the COM
//! owner thread. Fatal loss is stamped with the current capture's owner first.
use crate::dictation::{self, AppState};
use std::sync::{mpsc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use tauri::Emitter;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;
use windows_core::{implement, PCWSTR};

enum Signal { Changed, Shutdown }

pub struct MicrophoneMonitor {
    sender: mpsc::Sender<Signal>,
    worker: Mutex<Option<JoinHandle<()>>>,
}
impl Drop for MicrophoneMonitor {
    fn drop(&mut self) {
        self.shutdown();
    }
}
impl MicrophoneMonitor {
    pub fn shutdown(&self) {
        let _ = self.sender.send(Signal::Shutdown);
        if let Ok(mut worker) = self.worker.lock() && let Some(worker) = worker.take() { let _ = worker.join(); }
    }
}

#[implement(IMMNotificationClient)]
struct Notifications { sender: mpsc::Sender<Signal>, state: AppState }
impl Notifications {
    fn changed(&self) { let _ = self.sender.send(Signal::Changed); }
    fn lost(&self, id: &PCWSTR) {
        if let Ok(id) = unsafe { id.to_string() } { self.state.audio.invalidate_endpoint(&id); }
        self.changed();
    }
}
#[allow(non_snake_case)]
impl IMMNotificationClient_Impl for Notifications_Impl {
    fn OnDeviceStateChanged(&self, id: &PCWSTR, state: DEVICE_STATE) -> windows_core::Result<()> {
        if state != DEVICE_STATE_ACTIVE { self.lost(id); } else { self.changed(); }
        Ok(())
    }
    fn OnDeviceAdded(&self, _id: &PCWSTR) -> windows_core::Result<()> { self.changed(); Ok(()) }
    fn OnDeviceRemoved(&self, id: &PCWSTR) -> windows_core::Result<()> { self.lost(id); Ok(()) }
    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, _role: ERole, _id: &PCWSTR) -> windows_core::Result<()> {
        if flow == eCapture { self.changed(); }
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _id: &PCWSTR, _key: &PROPERTYKEY) -> windows_core::Result<()> { self.changed(); Ok(()) }
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DevicesChanged { generation: u64, devices: Vec<crate::audio::InputDeviceInfo> }

pub fn spawn(app: tauri::AppHandle, state: AppState) -> Result<MicrophoneMonitor, String> {
    let (sender, receiver) = mpsc::channel();
    let (ready, started) = mpsc::channel();
    let notifications_sender = sender.clone();
    let worker = std::thread::spawn(move || {
        unsafe {
            if let Err(error) = CoInitializeEx(None, COINIT_MULTITHREADED).ok() { let _ = ready.send(Err(error.to_string())); return; }
            let result = (|| -> windows_core::Result<()> {
                let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
                let callback: IMMNotificationClient = Notifications { sender: notifications_sender, state: state.clone() }.into();
                enumerator.RegisterEndpointNotificationCallback(&callback)?;
                let _ = ready.send(Ok(()));
                let mut generation = 0_u64;
                'events: while let Ok(signal) = receiver.recv() {
                    if matches!(signal, Signal::Shutdown) { break; }
                    // A quiet 120ms window collapses name/state/default bursts.
                    loop {
                        match receiver.recv_timeout(Duration::from_millis(120)) {
                            Ok(Signal::Shutdown) => break 'events,
                            Ok(Signal::Changed) => continue,
                            Err(mpsc::RecvTimeoutError::Timeout) => break,
                            Err(mpsc::RecvTimeoutError::Disconnected) => break 'events,
                        }
                    }
                    match dictation::input_device_choices(&state) {
                        Ok(devices) => {
                            generation = generation.wrapping_add(1);
                            let _ = app.emit("audio://devices-changed", DevicesChanged { generation, devices });
                            let locale = state.settings.read().ok().map(|settings| crate::resolved_language(&settings).to_owned());
                            if let Some(locale) = locale { crate::rebuild_tray_menu(&app, &state, &locale); }
                        }
                        Err(error) => { let _ = app.emit("dictation://persistence_error", format!("Cannot refresh microphones: {error}")); }
                    }
                }
                let result = enumerator.UnregisterEndpointNotificationCallback(&callback);
                drop(callback);
                drop(enumerator);
                result
            })();
            if let Err(error) = result { let _ = ready.send(Err(error.to_string())); }
            CoUninitialize();
        }
    });
    let monitor = MicrophoneMonitor { sender, worker: Mutex::new(Some(worker)) };
    match started.recv_timeout(Duration::from_secs(3)) {
        Ok(Ok(())) => Ok(monitor),
        Ok(Err(error)) => Err(error),
        Err(error) => Err(error.to_string()),
    }
}
