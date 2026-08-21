use crate::domain::DictationState;
use serde::{Deserialize, Serialize};
use std::fmt;
use tauri::{AppHandle, Manager, PhysicalPosition, Runtime};
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut as RegisteredShortcut};
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
    #[error("could not restore target focus: {0}")]
    FocusFailed(String),
    #[error("paste injection failed: {0}")]
    PasteFailed(String),
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
        windows.restore_foreground(target)?;
    }
    windows.send_ctrl_v()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutRole {
    Toggle,
    Cancel,
    Ignore,
}

pub fn shortcut_role(event: &str, configured_toggle: &str) -> Result<ShortcutRole, PlatformError> {
    let event = event
        .parse::<RegisteredShortcut>()
        .map_err(|error| PlatformError::InvalidShortcut(error.to_string()))?;
    shortcut_role_for(&event, configured_toggle)
}

pub fn shortcut_role_for(
    event: &RegisteredShortcut,
    configured_toggle: &str,
) -> Result<ShortcutRole, PlatformError> {
    let configured = configured_toggle
        .parse::<RegisteredShortcut>()
        .map_err(|error| PlatformError::InvalidShortcut(error.to_string()))?;
    let cancel = "Escape"
        .parse::<RegisteredShortcut>()
        .map_err(|error| PlatformError::InvalidShortcut(error.to_string()))?;
    Ok(if *event == configured {
        ShortcutRole::Toggle
    } else if *event == cancel {
        ShortcutRole::Cancel
    } else {
        ShortcutRole::Ignore
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutostartAction {
    Enable,
    Disable,
}

pub fn autostart_policy(launch_on_login: bool) -> AutostartAction {
    if launch_on_login {
        AutostartAction::Enable
    } else {
        AutostartAction::Disable
    }
}

pub fn sync_autostart<R: Runtime>(
    app: &AppHandle<R>,
    launch_on_login: bool,
) -> Result<(), PlatformError> {
    let manager = app.autolaunch();
    let enabled = manager
        .is_enabled()
        .map_err(|error| PlatformError::Other(error.to_string()))?;
    let should_enable = matches!(
        autostart_policy(launch_on_login),
        AutostartAction::Enable
    );
    if enabled == should_enable {
        return Ok(());
    }
    let result = if should_enable {
        manager.enable()
    } else {
        manager.disable()
    };
    result.map_err(|error| PlatformError::Other(error.to_string()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhysicalRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhysicalWindowSize {
    pub width: u32,
    pub height: u32,
}

pub fn bottom_center_position(work_area: PhysicalRect, window: PhysicalWindowSize) -> (i32, i32) {
    let width = i32::try_from(window.width).unwrap_or(i32::MAX);
    let height = i32::try_from(window.height).unwrap_or(i32::MAX);
    (
        work_area.left + ((work_area.right - work_area.left - width) / 2),
        work_area.bottom - height - 16,
    )
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
        .map_err(|error| PlatformError::ShortcutRegistration(error.to_string()))
}

/// The key that discards a recording in progress.
pub const CANCEL_SHORTCUT: &str = "Escape";

/// Claims or releases Escape.
///
/// A global hotkey is exclusive: while it is held, no other application sees
/// that key at all. Escape is far too important to hold permanently — doing so
/// breaks dialogs, menus and games system-wide — so it is claimed only while a
/// recording exists to discard, and released the moment one does not.
///
/// Both directions are idempotent, because this is driven from every state
/// change and most of those do not cross the boundary.
pub fn set_cancel_shortcut<R: Runtime>(
    app: &AppHandle<R>,
    armed: bool,
) -> Result<(), PlatformError> {
    let manager = app.global_shortcut();
    if armed == manager.is_registered(CANCEL_SHORTCUT) {
        return Ok(());
    }
    let result = if armed {
        manager.register(CANCEL_SHORTCUT)
    } else {
        manager.unregister(CANCEL_SHORTCUT)
    };
    result.map_err(|error| PlatformError::ShortcutRegistration(error.to_string()))
}

/// Temporarily unregisters all global shortcuts (used while the user records
/// a new shortcut in settings, so the old combo does not fire mid-capture).
pub fn suspend_shortcuts<R: Runtime>(app: &AppHandle<R>) -> Result<(), PlatformError> {
    app.global_shortcut()
        .unregister_all()
        .map_err(|error| PlatformError::ShortcutRegistration(error.to_string()))
}

pub fn replace_toggle_shortcut<R: Runtime>(
    app: &AppHandle<R>,
    old: &str,
    new: &str,
) -> Result<(), PlatformError> {
    let old = Shortcut::parse(old)?.to_string();
    let new = Shortcut::parse(new)?.to_string();
    if old == new {
        return Ok(());
    }
    let manager = app.global_shortcut();
    manager
        .register(new.as_str())
        .map_err(|error| PlatformError::ShortcutRegistration(error.to_string()))?;
    if let Err(error) = manager.unregister(old.as_str()) {
        let _ = manager.unregister(new.as_str());
        return Err(PlatformError::ShortcutRegistration(error.to_string()));
    }
    Ok(())
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
        #[cfg(windows)]
        {
            use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};
            if let Ok(hwnd) = window.hwnd() {
                unsafe {
                    ShowWindow(hwnd.0, SW_HIDE);
                }
                return;
            }
        }
        let _ = window.hide();
    }
}

fn overlay_origin(
    work: PhysicalRect,
    size: PhysicalWindowSize,
    position: Option<(i32, i32)>,
) -> (i32, i32) {
    position.unwrap_or_else(|| bottom_center_position(work, size))
}

pub fn show_overlay_without_focus<R: Runtime>(
    app: &AppHandle<R>,
    position: Option<(i32, i32)>,
) -> Result<(), PlatformError> {
    let window = app
        .get_webview_window("overlay")
        .ok_or_else(|| PlatformError::Other("overlay window is missing".into()))?;
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{SW_SHOWNOACTIVATE, ShowWindow};
        let size = window
            .outer_size()
            .map_err(|error| PlatformError::Other(error.to_string()))?;
        let (x, y) = overlay_origin(
            windows_work_area()?,
            PhysicalWindowSize {
                width: size.width,
                height: size.height,
            },
            position,
        );
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
fn windows_work_area() -> Result<PhysicalRect, PlatformError> {
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
    Ok(PhysicalRect {
        left: area.left,
        top: area.top,
        right: area.right,
        bottom: area.bottom,
    })
}

pub fn show_overlay_with_focus<R: Runtime>(
    app: &AppHandle<R>,
    position: Option<(i32, i32)>,
) -> Result<(), PlatformError> {
    let window = app
        .get_webview_window("overlay")
        .ok_or_else(|| PlatformError::Other("overlay window is missing".into()))?;
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SW_SHOW, SetForegroundWindow, ShowWindow,
        };
        let size = window
            .outer_size()
            .map_err(|error| PlatformError::Other(error.to_string()))?;
        let (x, y) = overlay_origin(
            windows_work_area()?,
            PhysicalWindowSize {
                width: size.width,
                height: size.height,
            },
            position,
        );
        window
            .set_position(PhysicalPosition::new(x, y))
            .map_err(|error| PlatformError::Other(error.to_string()))?;
        let hwnd = window
            .hwnd()
            .map_err(|error| PlatformError::Other(error.to_string()))?;
        unsafe {
            ShowWindow(hwnd.0, SW_SHOW);
            SetForegroundWindow(hwnd.0);
        }
        // SetForegroundWindow can be refused by the OS; Tauri's set_focus is
        // a best-effort fallback so the DOM receives the keydown.
        let _ = window.set_focus();
        Ok(())
    }
    #[cfg(not(windows))]
    window
        .show()
        .and_then(|()| window.set_focus())
        .map_err(|error| PlatformError::Other(error.to_string()))
}

pub fn restore_foreground_to<R: Runtime>(_app: &AppHandle<R>, target: Option<WindowTarget>) {
    if let Some(target) = target {
        let mut windows = SystemWindows;
        let _ = windows.restore_foreground(target);
    }
}

/// Reveal `path` in the system file manager (selecting the file/folder).
///
/// Used by the history and settings views so the user can find a recording or
/// a downloaded model on disk. The call resolves once the file manager has
/// been asked to open; it never blocks on the external process.
pub fn reveal_path(path: &std::path::Path) -> Result<(), PlatformError> {
    if !path.exists() {
        return Err(PlatformError::Other(format!(
            "ścieżka nie istnieje: {}",
            path.display()
        )));
    }
    let canonical = std::fs::canonicalize(path).map_err(|error| PlatformError::Other(error.to_string()))?;
    #[cfg(windows)]
    {
        use std::process::Command;
        // canonicalize() returns a `\\?\` verbatim path which explorer.exe
        // cannot parse and would open the default folder (Desktop) instead.
        let raw = canonical.as_os_str().to_string_lossy();
        let cleaned = raw.strip_prefix(r"\\?\").unwrap_or(&raw);
        let argument = format!("/select,\"{}\"", cleaned);
        Command::new("explorer")
            .arg(argument)
            .status()
            .map_err(|error| PlatformError::Other(error.to_string()))?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .args(["-R", &canonical.display().to_string()])
            .status()
            .map_err(|error| PlatformError::Other(error.to_string()))?;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // xdg-open has no "select" verb, so open the containing directory.
        let open_dir = if canonical.is_dir() {
            canonical.clone()
        } else {
            canonical.parent().map(std::path::Path::to_path_buf).unwrap_or(canonical.clone())
        };
        std::process::Command::new("xdg-open")
            .arg(open_dir)
            .status()
            .map_err(|error| PlatformError::Other(error.to_string()))?;
        Ok(())
    }
    #[cfg(not(any(windows, target_os = "macos", all(unix, not(target_os = "macos")))))]
    {
        Err(PlatformError::Other("reveal is not implemented on this platform".into()))
    }
}

pub struct SystemWindows;

impl WindowsApi for SystemWindows {
    fn foreground_window(&mut self) -> Option<WindowTarget> {
        #[cfg(windows)]
        {
            let hwnd = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
            return (!hwnd.is_null()).then_some(WindowTarget(hwnd as isize));
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    fn restore_foreground(&mut self, target: WindowTarget) -> Result<(), PlatformError> {
        #[cfg(windows)]
        {
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                SW_RESTORE, SetForegroundWindow, ShowWindow,
            };
            let hwnd = target.0 as windows_sys::Win32::Foundation::HWND;
            unsafe {
                ShowWindow(hwnd, SW_RESTORE);
                if SetForegroundWindow(hwnd) == 0 {
                    return Err(PlatformError::FocusFailed(
                        "Windows refused to restore the target foreground window".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    fn write_clipboard(&mut self, text: &str) -> Result<(), PlatformError> {
        let mut clipboard = arboard::Clipboard::new()
            .map_err(|error| PlatformError::ClipboardWrite(error.to_string()))?;
        clipboard
            .set_text(text.to_owned())
            .map_err(|error| PlatformError::ClipboardWrite(error.to_string()))
    }

    fn send_ctrl_v(&mut self) -> Result<(), PlatformError> {
        #[cfg(windows)]
        {
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
                return Err(PlatformError::PasteFailed(format!(
                    "SendInput accepted {sent}/{} events",
                    inputs.len()
                )));
            }
            Ok(())
        }
        #[cfg(target_os = "macos")]
        {
            let output = std::process::Command::new("osascript")
                .args([
                    "-e",
                    r#"tell application "System Events" to keystroke "v" using command down"#,
                ])
                .output()
                .map_err(|error| PlatformError::PasteFailed(error.to_string()))?;
            if !output.status.success() {
                return Err(PlatformError::PasteFailed(
                    String::from_utf8_lossy(&output.stderr).into_owned(),
                ));
            }
            Ok(())
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let output = std::process::Command::new("xdotool")
                .args(["key", "--clearmodifiers", "ctrl+v"])
                .output()
                .map_err(|error| PlatformError::PasteFailed(error.to_string()))?;
            if !output.status.success() {
                return Err(PlatformError::PasteFailed(
                    String::from_utf8_lossy(&output.stderr).into_owned(),
                ));
            }
            Ok(())
        }
        #[cfg(not(any(windows, target_os = "macos", all(unix, not(target_os = "macos")))))]
        {
            Err(PlatformError::PasteFailed(
                "paste injection is not implemented on this platform".into(),
            ))
        }
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
        fail_focus: bool,
        fail_paste: bool,
    }

    impl WindowsApi for FakeWindows {
        fn foreground_window(&mut self) -> Option<WindowTarget> {
            Some(WindowTarget(42))
        }

        fn restore_foreground(&mut self, target: WindowTarget) -> Result<(), PlatformError> {
            self.restored.push(target);
            if self.fail_focus {
                return Err(PlatformError::FocusFailed("denied".into()));
            }
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
                return Err(PlatformError::PasteFailed("SendInput".into()));
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
        assert!(matches!(result, Err(PlatformError::PasteFailed(_))));
    }

    #[test]
    fn failed_focus_restore_never_injects_paste() {
        let mut windows = FakeWindows {
            fail_focus: true,
            ..Default::default()
        };

        let result = copy_and_paste(&mut windows, Some(WindowTarget(9)), "tekst");

        assert!(matches!(result, Err(PlatformError::FocusFailed(_))));
        assert_eq!(windows.clipboard, vec!["tekst"]);
        assert_eq!(windows.paste_attempts, 0);
    }

    #[test]
    fn routes_registered_shortcuts_by_full_identity() {
        assert_eq!(
            shortcut_role("Ctrl+Escape", "Ctrl+Escape").unwrap(),
            ShortcutRole::Toggle
        );
        assert_eq!(
            shortcut_role("Escape", "Ctrl+Escape").unwrap(),
            ShortcutRole::Cancel
        );
        assert_eq!(
            shortcut_role("Alt+Escape", "Ctrl+Escape").unwrap(),
            ShortcutRole::Ignore
        );
    }

    #[test]
    fn autostart_policy_matches_launch_on_login_setting() {
        assert_eq!(autostart_policy(true), AutostartAction::Enable);
        assert_eq!(autostart_policy(false), AutostartAction::Disable);
    }

    #[test]
    fn positions_physical_overlay_for_common_dpi_scales() {
        let work = PhysicalRect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        assert_eq!(
            bottom_center_position(
                work,
                PhysicalWindowSize {
                    width: 312,
                    height: 56
                }
            ),
            (804, 968)
        );
        assert_eq!(
            bottom_center_position(
                work,
                PhysicalWindowSize {
                    width: 468,
                    height: 84
                }
            ),
            (726, 940)
        );
        assert_eq!(
            bottom_center_position(
                work,
                PhysicalWindowSize {
                    width: 624,
                    height: 112
                }
            ),
            (648, 912)
        );
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
