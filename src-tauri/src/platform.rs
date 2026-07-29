use crate::domain::DictationState;
use serde::{Deserialize, Serialize};
use std::fmt;
use tauri::{AppHandle, Manager, PhysicalPosition, Runtime};
use tauri_plugin_global_shortcut::GlobalShortcutExt;
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "code", content = "message", rename_all = "snake_case")]
pub enum PlatformError {
    #[error("invalid shortcut: {0}")]
    InvalidShortcut(String),
    #[error("shortcut registration failed: {0}")]
    ShortcutRegistration(String),
    #[error("clipboard write failed: {0}")]
    ClipboardWrite(String),
    #[error("paste injection failed: {0}")]
    PasteInjection(String),
    #[error("platform operation failed: {0}")]
    Other(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shortcut {
    ctrl: bool,
    alt: bool,
    shift: bool,
    meta: bool,
    key: String,
}

impl Shortcut {
    pub fn parse(input: &str) -> Result<Self, PlatformError> {
        let mut ctrl = false;
        let mut alt = false;
        let mut shift = false;
        let mut meta = false;
        let mut key = None;
        for raw in input.split('+') {
            let token = raw.trim();
            if token.is_empty() {
                return Err(PlatformError::InvalidShortcut(input.into()));
            }
            match token.to_ascii_lowercase().as_str() {
                "ctrl" | "control" if !ctrl => ctrl = true,
                "alt" if !alt => alt = true,
                "shift" if !shift => shift = true,
                "win" | "meta" | "super" if !meta => meta = true,
                "ctrl" | "control" | "alt" | "shift" | "win" | "meta" | "super" => {
                    return Err(PlatformError::InvalidShortcut(format!(
                        "duplicate modifier in {input}"
                    )));
                }
                _ if key.is_none() && valid_key(token) => key = Some(normalize_key(token)),
                _ => return Err(PlatformError::InvalidShortcut(input.into())),
            }
        }
        if !(ctrl || alt || shift || meta) {
            return Err(PlatformError::InvalidShortcut(
                "a global shortcut must include a modifier".into(),
            ));
        }
        Ok(Self {
            ctrl,
            alt,
            shift,
            meta,
            key: key.ok_or_else(|| PlatformError::InvalidShortcut(input.into()))?,
        })
    }
}

fn valid_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower == "space"
        || lower == "escape"
        || lower == "enter"
        || lower == "tab"
        || (key.len() == 1 && key.as_bytes()[0].is_ascii_alphanumeric())
        || lower
            .strip_prefix('f')
            .and_then(|number| number.parse::<u8>().ok())
            .is_some_and(|number| (1..=24).contains(&number))
}

fn normalize_key(key: &str) -> String {
    let lower = key.to_ascii_lowercase();
    match lower.as_str() {
        "space" => "Space".into(),
        "escape" => "Escape".into(),
        "enter" => "Enter".into(),
        "tab" => "Tab".into(),
        _ if lower.starts_with('f') => lower.to_ascii_uppercase(),
        _ => lower.to_ascii_uppercase(),
    }
}

impl fmt::Display for Shortcut {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("Ctrl");
        }
        if self.alt {
            parts.push("Alt");
        }
        if self.shift {
            parts.push("Shift");
        }
        if self.meta {
            parts.push("Meta");
        }
        parts.push(&self.key);
        formatter.write_str(&parts.join("+"))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowTarget(pub isize);

pub trait WindowsApi {
    fn foreground_window(&mut self) -> Option<WindowTarget>;
    fn restore_foreground(&mut self, target: WindowTarget) -> Result<(), PlatformError>;
    fn write_clipboard(&mut self, text: &str) -> Result<(), PlatformError>;
    fn send_ctrl_v(&mut self) -> Result<(), PlatformError>;
}

pub fn copy_and_paste(
    windows: &mut impl WindowsApi,
    target: Option<WindowTarget>,
    text: &str,
) -> Result<(), PlatformError> {
    windows.write_clipboard(text)?;
    if let Some(target) = target {
        let _ = windows.restore_foreground(target);
    }
    windows.send_ctrl_v()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlSource {
    Tray,
    GlobalShortcut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoordinatorAction {
    Start,
    Stop,
    Cancel,
    Ignore,
}

pub fn decide_action(_source: ControlSource, state: &DictationState) -> CoordinatorAction {
    match state {
        DictationState::Idle | DictationState::Failed { .. } => CoordinatorAction::Start,
        DictationState::Recording { .. } => CoordinatorAction::Stop,
        _ => CoordinatorAction::Ignore,
    }
}

pub fn decide_escape(state: &DictationState) -> CoordinatorAction {
    if matches!(state, DictationState::Recording { .. }) {
        CoordinatorAction::Cancel
    } else {
        CoordinatorAction::Ignore
    }
}

pub fn register_shortcuts<R: Runtime>(
    app: &AppHandle<R>,
    toggle: &str,
) -> Result<(), PlatformError> {
    let toggle = Shortcut::parse(toggle)?.to_string();
    let manager = app.global_shortcut();
    manager
        .unregister_all()
        .map_err(|error| PlatformError::ShortcutRegistration(error.to_string()))?;
    manager
        .register(toggle.as_str())
        .and_then(|()| manager.register("Escape"))
        .map_err(|error| PlatformError::ShortcutRegistration(error.to_string()))
}

pub fn show_main<R: Runtime>(app: &AppHandle<R>) -> Result<(), PlatformError> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| PlatformError::Other("main window is missing".into()))?;
    window
        .show()
        .and_then(|()| window.set_focus())
        .map_err(|error| PlatformError::Other(error.to_string()))
}

pub fn hide_overlay<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("overlay") {
        let _ = window.hide();
    }
}

pub fn show_overlay_without_focus<R: Runtime>(app: &AppHandle<R>) -> Result<(), PlatformError> {
    let window = app
        .get_webview_window("overlay")
        .ok_or_else(|| PlatformError::Other("overlay window is missing".into()))?;
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{SW_SHOWNOACTIVATE, ShowWindow};
        let (x, y) = windows_work_area_position(312, 56)?;
        window
            .set_position(PhysicalPosition::new(x, y))
            .map_err(|error| PlatformError::Other(error.to_string()))?;
        let hwnd = window
            .hwnd()
            .map_err(|error| PlatformError::Other(error.to_string()))?;
        unsafe {
            ShowWindow(hwnd.0, SW_SHOWNOACTIVATE);
        }
        Ok(())
    }
    #[cfg(not(windows))]
    window
        .show()
        .map_err(|error| PlatformError::Other(error.to_string()))
}

#[cfg(windows)]
fn windows_work_area_position(width: i32, height: i32) -> Result<(i32, i32), PlatformError> {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{SPI_GETWORKAREA, SystemParametersInfoW};
    let mut area = RECT::default();
    let succeeded = unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            (&raw mut area).cast(),
            Default::default(),
        )
    };
    if succeeded == 0 {
        return Err(PlatformError::Other("SPI_GETWORKAREA failed".into()));
    }
    Ok((
        area.left + ((area.right - area.left - width) / 2),
        area.bottom - height - 16,
    ))
}

#[cfg(windows)]
pub struct SystemWindows;

#[cfg(windows)]
impl WindowsApi for SystemWindows {
    fn foreground_window(&mut self) -> Option<WindowTarget> {
        let hwnd = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
        (!hwnd.is_null()).then_some(WindowTarget(hwnd as isize))
    }

    fn restore_foreground(&mut self, target: WindowTarget) -> Result<(), PlatformError> {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SW_RESTORE, SetForegroundWindow, ShowWindow,
        };
        let hwnd = target.0 as windows_sys::Win32::Foundation::HWND;
        unsafe {
            ShowWindow(hwnd, SW_RESTORE);
            if SetForegroundWindow(hwnd) == 0 {
                return Err(PlatformError::Other(
                    "Windows refused to restore the target foreground window".into(),
                ));
            }
        }
        Ok(())
    }

    fn write_clipboard(&mut self, text: &str) -> Result<(), PlatformError> {
        use std::ptr;
        use windows_sys::Win32::Foundation::GlobalFree;
        use windows_sys::Win32::System::DataExchange::{
            CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
        };
        use windows_sys::Win32::System::Memory::{
            GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock,
        };
        use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            if OpenClipboard(ptr::null_mut()) == 0 {
                return Err(PlatformError::ClipboardWrite("OpenClipboard".into()));
            }
            if EmptyClipboard() == 0 {
                CloseClipboard();
                return Err(PlatformError::ClipboardWrite("EmptyClipboard".into()));
            }
            let allocation = GlobalAlloc(GMEM_MOVEABLE, wide.len() * size_of::<u16>());
            if allocation.is_null() {
                CloseClipboard();
                return Err(PlatformError::ClipboardWrite("GlobalAlloc".into()));
            }
            let destination = GlobalLock(allocation).cast::<u16>();
            if destination.is_null() {
                GlobalFree(allocation);
                CloseClipboard();
                return Err(PlatformError::ClipboardWrite("GlobalLock".into()));
            }
            ptr::copy_nonoverlapping(wide.as_ptr(), destination, wide.len());
            GlobalUnlock(allocation);
            if SetClipboardData(u32::from(CF_UNICODETEXT), allocation).is_null() {
                GlobalFree(allocation);
                CloseClipboard();
                return Err(PlatformError::ClipboardWrite("SetClipboardData".into()));
            }
            CloseClipboard();
        }
        Ok(())
    }

    fn send_ctrl_v(&mut self) -> Result<(), PlatformError> {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VK_CONTROL,
        };
        fn key_input(key: u16, flags: u32) -> INPUT {
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: key,
                        wScan: 0,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            }
        }
        let inputs = [
            key_input(VK_CONTROL, 0),
            key_input(u16::from(b'V'), 0),
            key_input(u16::from(b'V'), KEYEVENTF_KEYUP),
            key_input(VK_CONTROL, KEYEVENTF_KEYUP),
        ];
        let sent = unsafe {
            SendInput(
                inputs.len().try_into().unwrap_or(u32::MAX),
                inputs.as_ptr(),
                size_of::<INPUT>().try_into().unwrap_or(i32::MAX),
            )
        };
        if sent != inputs.len() as u32 {
            return Err(PlatformError::PasteInjection(format!(
                "SendInput accepted {sent}/{} events",
                inputs.len()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_normalizes_supported_shortcuts() {
        assert_eq!(
            Shortcut::parse(" shift + ctrl + space ")
                .unwrap()
                .to_string(),
            "Ctrl+Shift+Space"
        );
        assert_eq!(Shortcut::parse("ALT+F12").unwrap().to_string(), "Alt+F12");
        assert!(matches!(
            Shortcut::parse("Ctrl+Ctrl+A"),
            Err(PlatformError::InvalidShortcut(_))
        ));
        assert!(matches!(
            Shortcut::parse("Space"),
            Err(PlatformError::InvalidShortcut(_))
        ));
    }

    #[derive(Default)]
    struct FakeWindows {
        clipboard: Vec<String>,
        restored: Vec<WindowTarget>,
        paste_attempts: usize,
        fail_clipboard: bool,
        fail_paste: bool,
    }

    impl WindowsApi for FakeWindows {
        fn foreground_window(&mut self) -> Option<WindowTarget> {
            Some(WindowTarget(42))
        }

        fn restore_foreground(&mut self, target: WindowTarget) -> Result<(), PlatformError> {
            self.restored.push(target);
            Ok(())
        }

        fn write_clipboard(&mut self, text: &str) -> Result<(), PlatformError> {
            if self.fail_clipboard {
                return Err(PlatformError::ClipboardWrite("locked".into()));
            }
            self.clipboard.push(text.to_owned());
            Ok(())
        }

        fn send_ctrl_v(&mut self) -> Result<(), PlatformError> {
            self.paste_attempts += 1;
            if self.fail_paste {
                return Err(PlatformError::PasteInjection("SendInput".into()));
            }
            Ok(())
        }
    }

    #[test]
    fn clipboard_success_and_paste_failure_are_distinct() {
        let mut windows = FakeWindows {
            fail_paste: true,
            ..Default::default()
        };

        let result = copy_and_paste(&mut windows, Some(WindowTarget(9)), "Zażółć");

        assert_eq!(windows.clipboard, vec!["Zażółć"]);
        assert_eq!(windows.restored, vec![WindowTarget(9)]);
        assert_eq!(windows.paste_attempts, 1);
        assert!(matches!(result, Err(PlatformError::PasteInjection(_))));
    }

    #[test]
    fn command_decision_uses_same_toggle_flow_for_tray_and_shortcut() {
        assert_eq!(
            decide_action(ControlSource::Tray, &DictationState::Idle),
            CoordinatorAction::Start
        );
        assert_eq!(
            decide_action(
                ControlSource::GlobalShortcut,
                &DictationState::Recording {
                    recording_id: "id".into(),
                    audio_path: "a.wav".into(),
                }
            ),
            CoordinatorAction::Stop
        );
        assert_eq!(
            decide_escape(&DictationState::Processing {
                recording_id: "id".into(),
                audio_path: "a.wav".into(),
            }),
            CoordinatorAction::Ignore
        );
    }
}
