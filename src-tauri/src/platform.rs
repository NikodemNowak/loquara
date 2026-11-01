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
#[serde(rename_all = "snake_case")]
pub enum PasteMode {
    #[default]
    Auto,
    CtrlV,
    CtrlShiftV,
    ShiftInsert,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowTarget(pub isize);

/// Snapshot of the full registered chord. The plugin's Released event only
/// observes its main key, so it cannot own the lifetime of a hold gesture.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ShortcutKeys {
    pub chord_down: bool,
    pub main_down: bool,
    pub escape_down: bool,
}

impl Shortcut {
    pub(crate) fn key_state(&self) -> Result<ShortcutKeys, PlatformError> {
        input_desktop_available()?;
        Ok(self.sample_keys(key_down))
    }
    fn sample_keys(&self, down: impl Fn(i32) -> bool) -> ShortcutKeys {
        let key = match self.key.as_str() {
            "Space" => 0x20, "Escape" => 0x1b, "Enter" => 0x0d, "Tab" => 0x09,
            key if key.starts_with('F') && key.len() > 1 => 0x70 + key[1..].parse::<i32>().unwrap_or(1) - 1,
            key => i32::from(key.as_bytes()[0]),
        };
        let main_down = down(key);
        ShortcutKeys {
            chord_down: main_down && (!self.ctrl || down(0x11)) && (!self.alt || down(0x12))
                && (!self.shift || down(0x10)) && (!self.meta || down(0x5b) || down(0x5c)),
            main_down,
            // A binding containing Escape must not cancel itself.
            escape_down: key != 0x1b && down(0x1b),
        }
    }
}

fn key_down(key: i32) -> bool {
    #[cfg(windows)]
    { unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(key) < 0 } }
    #[cfg(not(windows))]
    { let _ = key; false }
}

/// Zero from GetAsyncKeyState is ambiguous on an inaccessible desktop. Check
/// session and desktop before interpreting zero as a released physical key.
fn input_desktop_available() -> Result<(), PlatformError> {
    let unavailable = || PlatformError::PasteFailed("input desktop unavailable; final text is in the clipboard".into());
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::RemoteDesktop::*;
        use windows_sys::Win32::System::StationsAndDesktops::*;
        use windows_sys::Win32::System::Threading::GetCurrentThreadId;
        let mut buffer = std::ptr::null_mut();
        let mut bytes = 0;
        if WTSQuerySessionInformationW(WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION, WTSConnectState, &mut buffer, &mut bytes) == 0 { return Err(unavailable()); }
        let active = !buffer.is_null() && bytes >= 4 && *(buffer as *const i32) == WTSActive;
        WTSFreeMemory(buffer.cast());
        if !active { return Err(unavailable()); }
        let desktop = OpenInputDesktop(0, 0, DESKTOP_READOBJECTS);
        if desktop.is_null() { return Err(unavailable()); }
        let mut input_name = [0u16; 256];
        let mut thread_name = [0u16; 256];
        let mut needed = 0;
        let input_ok = GetUserObjectInformationW(desktop, UOI_NAME, input_name.as_mut_ptr().cast(), 512, &mut needed) != 0;
        let thread_desktop = GetThreadDesktop(GetCurrentThreadId());
        let thread_ok = !thread_desktop.is_null() && GetUserObjectInformationW(thread_desktop, UOI_NAME, thread_name.as_mut_ptr().cast(), 512, &mut needed) != 0;
        CloseDesktop(desktop);
        if !input_ok || !thread_ok || input_name != thread_name
            || windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow().is_null() { return Err(unavailable()); }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = unavailable;
        // The other desktops do not expose a locked input desktop the way
        // Windows does, so there is nothing to refuse here.
        Ok(())
    }
}

pub fn wait_for_modifiers(windows: &mut impl WindowsApi, shortcut: Option<&Shortcut>, cancelled: impl Fn() -> bool, trigger_released: impl Fn() -> bool, timeout: std::time::Duration) -> Result<(), PlatformError> {
    let started = std::time::Instant::now();
    loop {
        if cancelled() { return Err(PlatformError::PasteFailed("delivery cancelled".into())); }
        if windows.paste_keys_released(shortcut)? && trigger_released() { return Ok(()); }
        if started.elapsed() >= timeout {
            return Err(PlatformError::PasteFailed("modifier keys are still held; final text is in the clipboard".into()));
        }
        windows.pause_for_modifiers();
    }
}

pub trait WindowsApi {
    fn modifiers_released(&mut self) -> Result<bool, PlatformError> { Ok(true) }
    fn paste_keys_released(&mut self, _shortcut: Option<&Shortcut>) -> Result<bool, PlatformError> { self.modifiers_released() }
    fn pause_for_modifiers(&mut self) { std::thread::sleep(std::time::Duration::from_millis(10)); }
    fn foreground_window(&mut self) -> Option<WindowTarget>;
    fn restore_foreground(&mut self, target: WindowTarget) -> Result<(), PlatformError>;
    fn write_clipboard(&mut self, text: &str) -> Result<(), PlatformError>;
    fn send_ctrl_v(&mut self) -> Result<(), PlatformError>;
    fn send_ctrl_shift_v(&mut self) -> Result<(), PlatformError> {
        self.send_ctrl_v()
    }
    fn send_shift_insert(&mut self) -> Result<(), PlatformError> {
        self.send_ctrl_v()
    }
    fn send_paste(&mut self, target: Option<WindowTarget>, mode: PasteMode) -> Result<(), PlatformError> {
        let _ = target;
        match mode {
            PasteMode::Auto => self.send_ctrl_v(),
            PasteMode::CtrlV => self.send_ctrl_v(),
            PasteMode::CtrlShiftV => self.send_ctrl_shift_v(),
            PasteMode::ShiftInsert => self.send_shift_insert(),
        }
    }
}

fn require_released_modifiers(windows: &mut impl WindowsApi, shortcut: Option<&Shortcut>) -> Result<(), PlatformError> {
    if windows.paste_keys_released(shortcut)? { Ok(()) }
    else { Err(PlatformError::PasteFailed("modifier keys are still held; final text is in the clipboard".into())) }
}

pub fn copy_and_paste(
    windows: &mut impl WindowsApi,
    target: Option<WindowTarget>,
    text: &str,
    mode: PasteMode,
) -> Result<(), PlatformError> {
    copy_and_paste_cancellable(windows, target, text, mode, None, || false)
}

pub fn copy_and_paste_cancellable(
    windows: &mut impl WindowsApi, target: Option<WindowTarget>, text: &str, mode: PasteMode,
    shortcut: Option<&Shortcut>, cancelled: impl Fn() -> bool,
) -> Result<(), PlatformError> {
    let check_cancel = || if cancelled() { Err(PlatformError::PasteFailed("delivery cancelled; final text is in the clipboard".into())) } else { Ok(()) };
    check_cancel()?;
    windows.write_clipboard(text)?;
    if text.trim().is_empty() { return Ok(()); }
    let target = target.filter(|target| target.0 != 0);
    // Windows remembers the window that was in front before the overlay
    // appeared and insists on getting it back. macOS and the X11 desktops
    // keep the target focused by using an overlay that never takes focus, so
    // there is nothing to restore and no id to require.
    #[cfg(windows)]
    let target = target.ok_or_else(|| PlatformError::FocusFailed("target unavailable; final text is in the clipboard".into()))?;
    require_released_modifiers(windows, shortcut)?;
    check_cancel()?;
    #[cfg(windows)]
    windows.restore_foreground(target)?;
    // Focus restoration can wait. Both physical keys and cancellation may
    // have changed while it was in progress; never inject using stale state.
    require_released_modifiers(windows, shortcut)?;
    check_cancel()?;
    #[cfg(windows)]
    return windows.send_paste(Some(target), mode);
    #[cfg(not(windows))]
    windows.send_paste(target, mode)
}

/// Removes the last `count` characters from whatever window has focus.
///
/// Used by live dictation to take back a word the model changed its mind
/// about. It only ever runs immediately after this session typed those
/// characters itself.
pub fn backspace(count: usize) -> Result<(), PlatformError> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            INPUT, KEYEVENTF_KEYUP, SendInput, VK_BACK,
        };
        if count == 0 {
            return Ok(());
        }
        // Small batches, so a window that is slow to process input cannot
        // swallow the whole correction.
        for batch in 0..count.div_ceil(24) {
            let events: Vec<INPUT> = (0..24.min(count - batch * 24))
                .flat_map(|_| [key_input(VK_BACK, 0), key_input(VK_BACK, KEYEVENTF_KEYUP)])
                .collect();
            let sent = unsafe {
                SendInput(
                    events.len().try_into().unwrap_or(u32::MAX),
                    events.as_ptr(),
                    size_of::<INPUT>().try_into().unwrap_or(i32::MAX),
                )
            };
            if sent != events.len() as u32 {
                return Err(PlatformError::PasteFailed(format!(
                    "SendInput removed {sent}/{} events",
                    events.len()
                )));
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = count;
        Err(PlatformError::PasteFailed(
            "live typing is not implemented on this platform".into(),
        ))
    }
}

/// Types text into whatever window has focus, without touching the clipboard.
///
/// Live dictation arrives in pieces while the user is speaking, so each piece
/// has to land where the caret is without stealing the clipboard or the
/// focus from the application being dictated into.
pub fn type_text(text: &str) -> Result<(), PlatformError> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, SendInput,
        };
        if text.is_empty() {
            return Ok(());
        }
        let mut events: Vec<INPUT> = Vec::with_capacity(text.len() * 2);
        for unit in text.encode_utf16() {
            for flags in [KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP] {
                events.push(INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: 0,
                            wScan: unit,
                            dwFlags: flags,
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                });
            }
        }
        // Batched so one refused call cannot swallow a long piece, with a
        // breath between batches so a busy application can keep up.
        for batch in events.chunks(64) {
            let sent = unsafe {
                SendInput(
                    batch.len().try_into().unwrap_or(u32::MAX),
                    batch.as_ptr(),
                    size_of::<INPUT>().try_into().unwrap_or(i32::MAX),
                )
            };
            if sent != batch.len() as u32 {
                return Err(PlatformError::PasteFailed(format!(
                    "SendInput typed {sent}/{} events",
                    batch.len()
                )));
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = text;
        Err(PlatformError::PasteFailed(
            "live typing is not implemented on this platform".into(),
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutRole {
    Toggle,
    /// Escape: asks to discard, or takes the question back.
    Cancel,
    /// Enter: answers the question with yes.
    ConfirmCancel,
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
    let cancel = CANCEL_SHORTCUT
        .parse::<RegisteredShortcut>()
        .map_err(|error| PlatformError::InvalidShortcut(error.to_string()))?;
    let confirm = CONFIRM_SHORTCUT
        .parse::<RegisteredShortcut>()
        .map_err(|error| PlatformError::InvalidShortcut(error.to_string()))?;
    Ok(if *event == configured {
        ShortcutRole::Toggle
    } else if *event == cancel {
        ShortcutRole::Cancel
    } else if *event == confirm {
        ShortcutRole::ConfirmCancel
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

/// Keeps a remembered pill on the visible work area. A second monitor that
/// went away, or a drag that ended off-screen, otherwise leaves the overlay
/// shown and unreachable.
pub fn clamp_overlay_position(
    work_area: PhysicalRect,
    window: PhysicalWindowSize,
    x: i32,
    y: i32,
) -> (i32, i32) {
    let width = i32::try_from(window.width).unwrap_or(i32::MAX);
    let height = i32::try_from(window.height).unwrap_or(i32::MAX);
    let max_x = work_area.right.saturating_sub(width).max(work_area.left);
    let max_y = work_area.bottom.saturating_sub(height).max(work_area.top);
    (x.clamp(work_area.left, max_x), y.clamp(work_area.top, max_y))
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
    match state {
        DictationState::Recording { .. }
        | DictationState::Processing { .. }
        | DictationState::Cancelling { .. } => CoordinatorAction::Cancel,
        _ => CoordinatorAction::Ignore,
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

/// The key that asks to discard a recording, and takes the question back.
pub const CANCEL_SHORTCUT: &str = "Escape";
/// The key that answers that question with yes.
pub const CONFIRM_SHORTCUT: &str = "Enter";

/// Claims or releases the keys the recorder answers to.
///
/// A global hotkey is exclusive: while it is held, no other application sees
/// that key at all. Escape and Enter are far too important to hold
/// permanently — doing so breaks dialogs, menus and games system-wide — so
/// each is claimed only while it has something to do. Escape lives as long as
/// there is a recording to discard; Enter only while the question is on
/// screen waiting for an answer.
///
/// Every direction is idempotent, because this is driven from every state
/// change and most of those do not cross a boundary.
pub fn arm_recorder_keys<R: Runtime>(
    app: &AppHandle<R>,
    cancel: bool,
    confirm: bool,
) -> Result<(), PlatformError> {
    set_shortcut(app, CANCEL_SHORTCUT, cancel)?;
    set_shortcut(app, CONFIRM_SHORTCUT, confirm)
}

fn set_shortcut<R: Runtime>(
    app: &AppHandle<R>,
    accelerator: &str,
    armed: bool,
) -> Result<(), PlatformError> {
    let manager = app.global_shortcut();
    if armed == manager.is_registered(accelerator) {
        return Ok(());
    }
    let result = if armed {
        manager.register(accelerator)
    } else {
        manager.unregister(accelerator)
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
    // Showing the window is the part that must work; the rest is manners.
    // Windows refuses to hand focus to a process that is not the foreground
    // one, which is ordinary rather than exceptional — and chaining it made
    // that refusal fail the whole call. At startup that left the window
    // hidden and the app running with nothing on screen.
    window
        .show()
        .map_err(|error| PlatformError::Other(error.to_string()))?;
    // Hidden and minimised are different states, and a window can be in both:
    // showing a minimised window leaves it in the taskbar.
    let _ = window.unminimize();
    let _ = window.set_focus();
    Ok(())
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
    visible: PhysicalRect,
    size: PhysicalWindowSize,
    position: Option<(i32, i32)>,
) -> (i32, i32) {
    match position {
        Some((x, y)) => clamp_overlay_position(visible, size, x, y),
        None => bottom_center_position(work, size),
    }
}

/// Visible, topmost, and not the keyboard target.
///
/// `SW_SHOWNOACTIVATE` alone leaves the HWND where it was in the Z-order —
/// usually under the app the user is dictating into. Pinning `HWND_TOPMOST`
/// with `SWP_NOACTIVATE` is the Win32 way to float without stealing focus.
#[cfg(windows)]
fn show_overlay_hwnd_topmost(hwnd: windows_sys::Win32::Foundation::HWND, activate: bool) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_SHOWWINDOW,
        SW_SHOW, SW_SHOWNOACTIVATE, SetForegroundWindow, SetWindowPos, ShowWindow,
    };
    let mut flags = SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW | SWP_NOOWNERZORDER;
    if !activate {
        flags |= SWP_NOACTIVATE;
    }
    unsafe {
        ShowWindow(hwnd, if activate { SW_SHOW } else { SW_SHOWNOACTIVATE });
        SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, flags);
        if activate {
            SetForegroundWindow(hwnd);
        }
    }
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
        let size = window
            .outer_size()
            .map_err(|error| PlatformError::Other(error.to_string()))?;
        let work = windows_monitor_work_area(position)?;
        let (x, y) = overlay_origin(
            work,
            work,
            PhysicalWindowSize {
                width: size.width,
                height: size.height,
            },
            position,
        );
        window
            .set_position(PhysicalPosition::new(x, y))
            .map_err(|error| PlatformError::Other(error.to_string()))?;
        let _ = window.set_always_on_top(true);
        let hwnd = window
            .hwnd()
            .map_err(|error| PlatformError::Other(error.to_string()))?;
        show_overlay_hwnd_topmost(hwnd.0, false);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = window.set_always_on_top(true);
        window
            .show()
            .map_err(|error| PlatformError::Other(error.to_string()))
    }
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

/// Select the actual monitor nearest the remembered origin (or the target
/// foreground window), excluding its taskbar. A virtual-desktop union includes
/// invisible gaps between displays and is not a safe placement boundary.
#[cfg(windows)]
fn windows_monitor_work_area(position: Option<(i32, i32)>) -> Result<PhysicalRect, PlatformError> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    let monitor = unsafe {
        match position {
            Some((x, y)) => MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST),
            None => MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTONEAREST),
        }
    };
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 { return windows_work_area(); }
    Ok(PhysicalRect { left: info.rcWork.left, top: info.rcWork.top, right: info.rcWork.right, bottom: info.rcWork.bottom })
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
        let size = window
            .outer_size()
            .map_err(|error| PlatformError::Other(error.to_string()))?;
        let work = windows_monitor_work_area(position)?;
        let (x, y) = overlay_origin(
            work,
            work,
            PhysicalWindowSize {
                width: size.width,
                height: size.height,
            },
            position,
        );
        window
            .set_position(PhysicalPosition::new(x, y))
            .map_err(|error| PlatformError::Other(error.to_string()))?;
        let _ = window.set_always_on_top(true);
        let hwnd = window
            .hwnd()
            .map_err(|error| PlatformError::Other(error.to_string()))?;
        show_overlay_hwnd_topmost(hwnd.0, true);
        // SetForegroundWindow can be refused by the OS; Tauri's set_focus is
        // a best-effort fallback so the DOM receives the keydown.
        let _ = window.set_focus();
        Ok(())
    }
    #[cfg(not(windows))]
    // Hidden and minimised are different states, and a window can be in both:
    // showing a minimised window leaves it in the taskbar.
    window
        .show()
        .and_then(|()| window.unminimize())
        .and_then(|()| window.set_focus())
        .map_err(|error| PlatformError::Other(error.to_string()))
}

/// Bytes still free on the volume `path` lives on.
///
/// `None` when the platform will not say, which callers treat as "no reason
/// to refuse" rather than as zero.
#[cfg(windows)]
pub fn free_space_bytes(path: &std::path::Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut available: u64 = 0;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(available)
}

#[cfg(not(windows))]
pub fn free_space_bytes(_path: &std::path::Path) -> Option<u64> {
    None
}

pub fn restore_foreground_to<R: Runtime>(_app: &AppHandle<R>, target: Option<WindowTarget>) {
    if let Some(target) = target {
        let mut windows = SystemWindows;
        let _ = windows.restore_foreground(target);
    }
}

/// Builds the command line that shows `path` in Windows Explorer.
///
/// A directory is opened; a file is revealed with its parent open and the file
/// selected. `canonicalize` returns a verbatim `\\?\` path, which Explorer does
/// not understand and answers by opening the default folder, so the prefix is
/// stripped first.
#[cfg(windows)]
fn explorer_argument(canonical: &std::path::Path, is_dir: bool) -> String {
    let raw = canonical.as_os_str().to_string_lossy();
    let cleaned = raw.strip_prefix(r"\\?\").unwrap_or(&raw);
    if is_dir {
        format!("\"{cleaned}\"")
    } else {
        format!("/select,\"{cleaned}\"")
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
        use std::os::windows::process::CommandExt;
        use std::process::Command;
        // raw_arg, not arg: Explorer parses its own command line, and the
        // escaping Rust applies to a normal argument leaves it with a string
        // it cannot read — at which point it silently opens Documents.
        Command::new("explorer")
            .raw_arg(explorer_argument(&canonical, path.is_dir()))
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

#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalPasteType {
    None,
    CtrlShiftV,
    ShiftInsert,
}

#[cfg(windows)]
pub fn detect_terminal_paste_type(hwnd: windows_sys::Win32::Foundation::HWND) -> TerminalPasteType {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetWindowTextW, GetWindowThreadProcessId,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::Foundation::CloseHandle;

    if hwnd.is_null() {
        return TerminalPasteType::None;
    }

    // 1. Check window class name
    let mut class_buf = [0u16; 256];
    let class_len = unsafe { GetClassNameW(hwnd, class_buf.as_mut_ptr(), class_buf.len() as i32) };
    let class_name = if class_len > 0 {
        String::from_utf16_lossy(&class_buf[..class_len as usize]).to_ascii_lowercase()
    } else {
        String::new()
    };

    if class_name.contains("putty") || class_name.contains("kitty") {
        return TerminalPasteType::ShiftInsert;
    }
    if class_name == "cascadia_hosting_window_class" // Windows Terminal
        || class_name == "consolewindowclass"         // conhost / cmd / powershell
        || class_name == "mintty"                     // Git Bash / Cygwin / MSYS2
        || class_name.contains("alacritty")
        || class_name.contains("wezterm")
        || class_name.contains("ghostty")
        || class_name.contains("virtualconsoleclass") // ConEmu
        || class_name.contains("conemumain")
        || class_name.contains("mobaxterm")
        || class_name.contains("tmoba")
        || class_name.contains("vcxsrv")             // X11 SSH / Remote Linux GUI
        || class_name.contains("x410")
        || class_name.contains("x-frame")
        || class_name.contains("rail_window")         // WSLg Remote App / Terminal
        || class_name.contains("wsldvcwindowclass")
    {
        return TerminalPasteType::CtrlShiftV;
    }

    // 2. Check window title
    let mut title_buf = [0u16; 512];
    let title_len = unsafe { GetWindowTextW(hwnd, title_buf.as_mut_ptr(), title_buf.len() as i32) };
    if title_len > 0 {
        let title = String::from_utf16_lossy(&title_buf[..title_len as usize]).to_ascii_lowercase();
        if title.contains("termius") {
            return TerminalPasteType::CtrlShiftV;
        }
        if title.contains("putty") {
            return TerminalPasteType::ShiftInsert;
        }
    }

    // 3. Check process executable name
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid != 0 {
        let process_handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if !process_handle.is_null() {
            let mut name_buf = [0u16; 1024];
            let mut size = name_buf.len() as u32;
            let ok = unsafe {
                QueryFullProcessImageNameW(process_handle, 0, name_buf.as_mut_ptr(), &mut size)
            };
            unsafe { CloseHandle(process_handle) };
            if ok != 0 && size > 0 {
                let full_path = String::from_utf16_lossy(&name_buf[..size as usize]).to_ascii_lowercase();
                let exe_name = full_path.rsplit('\\').next().unwrap_or(&full_path);

                if exe_name.contains("putty") || exe_name.contains("kitty") {
                    return TerminalPasteType::ShiftInsert;
                }
                if exe_name.contains("termius")
                    || exe_name.contains("windowsterminal")
                    || exe_name == "wt.exe"
                    || exe_name.contains("alacritty")
                    || exe_name.contains("wezterm")
                    || exe_name.contains("ghostty")
                    || exe_name.contains("mintty")
                    || exe_name.contains("conhost")
                    || exe_name.contains("powershell")
                    || exe_name == "pwsh.exe"
                    || exe_name == "cmd.exe"
                    || exe_name.contains("mobaxterm")
                    || exe_name.contains("wsl")
                    || exe_name.contains("tabby")
                    || exe_name.contains("terminus")
                    || exe_name.contains("hyper")
                    || exe_name.contains("securecrt")
                    || exe_name.contains("xshell")
                    || exe_name.contains("vcxsrv")
                    || exe_name.contains("x410")
                    || exe_name.contains("xming")
                    || exe_name.contains("wslg")
                {
                    return TerminalPasteType::CtrlShiftV;
                }
            }
        }
    }

    TerminalPasteType::None
}

#[cfg(all(unix, not(target_os = "macos")))]
fn xdotool_key(chord: &str) -> Result<(), PlatformError> {
    let output = std::process::Command::new("xdotool")
        .args(["key", "--clearmodifiers", chord])
        .output()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => PlatformError::PasteFailed(
                "automatic paste on Linux needs xdotool: install it with your package manager. \
                 The transcript is already in the clipboard."
                    .into(),
            ),
            _ => PlatformError::PasteFailed(error.to_string()),
        })?;
    if !output.status.success() {
        return Err(PlatformError::PasteFailed(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    Ok(())
}

/// Whether the focused X11 window looks like a terminal.
///
/// Terminals put paste on Ctrl+Shift+V (or Shift+Insert), while every other
/// application uses Ctrl+V. Asking xdotool for the window class is the only
/// portable way to tell them apart without maintaining a list of processes
/// that drifts as soon as somebody installs a new emulator.
#[cfg(all(unix, not(target_os = "macos")))]
fn xdotool_terminal_active() -> bool {
    let Ok(output) = std::process::Command::new("xdotool")
        .args(["getactivewindow", "getwindowclassname"])
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let class = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    const TERMINALS: &[&str] = &[
        "gnome-terminal",
        "kgx",
        "konsole",
        "xterm",
        "kitty",
        "alacritty",
        "wezterm",
        "terminator",
        "tilix",
        "xfce4-terminal",
        "mate-terminal",
        "lxterminal",
        "qterminal",
        "terminology",
        "urxvt",
        "st-",
        "tabby",
        "warp",
    ];
    TERMINALS.iter().any(|terminal| class.contains(terminal))
}

#[cfg(target_os = "macos")]
fn macos_keystroke(script: &str) -> Result<(), PlatformError> {
    let output = std::process::Command::new("osascript")
        .args(["-e", script])
        .output()
        .map_err(|error| PlatformError::PasteFailed(error.to_string()))?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(PlatformError::PasteFailed(format!(
        "macOS refused the keystroke{}. Allow Loquara in System Settings → Privacy & Security → \
         Accessibility, then try again. The transcript is already in the clipboard.",
        if detail.is_empty() { String::new() } else { format!(": {detail}") }
    )))
}

#[cfg(windows)]
fn send_inputs_with_hold(
    down: &[windows_sys::Win32::UI::Input::KeyboardAndMouse::INPUT],
    up: &[windows_sys::Win32::UI::Input::KeyboardAndMouse::INPUT],
) -> Result<(), PlatformError> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{SendInput, INPUT};

    let sent_down = unsafe {
        SendInput(
            down.len().try_into().unwrap_or(u32::MAX),
            down.as_ptr(),
            size_of::<INPUT>().try_into().unwrap_or(i32::MAX),
        )
    };
    if sent_down != down.len() as u32 {
        return Err(PlatformError::PasteFailed(format!(
            "SendInput down accepted {sent_down}/{} events",
            down.len()
        )));
    }

    std::thread::sleep(std::time::Duration::from_millis(15));

    let sent_up = unsafe {
        SendInput(
            up.len().try_into().unwrap_or(u32::MAX),
            up.as_ptr(),
            size_of::<INPUT>().try_into().unwrap_or(i32::MAX),
        )
    };
    if sent_up != up.len() as u32 {
        return Err(PlatformError::PasteFailed(format!(
            "SendInput up accepted {sent_up}/{} events",
            up.len()
        )));
    }

    Ok(())
}

#[cfg(windows)]
fn key_input(key: u16, flags: u32) -> windows_sys::Win32::UI::Input::KeyboardAndMouse::INPUT {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, MapVirtualKeyW,
        VK_INSERT,
    };
    let scan = unsafe { MapVirtualKeyW(key as u32, 0) } as u16;
    let extra_flags = if key == VK_INSERT { KEYEVENTF_EXTENDEDKEY } else { 0 };
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                wScan: scan,
                dwFlags: flags | extra_flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

pub struct SystemWindows;

impl WindowsApi for SystemWindows {
    fn paste_keys_released(&mut self, shortcut: Option<&Shortcut>) -> Result<bool, PlatformError> {
        if !self.modifiers_released()? { return Ok(false); }
        match shortcut { Some(shortcut) => Ok(!shortcut.key_state()?.main_down), None => Ok(true) }
    }

    fn modifiers_released(&mut self) -> Result<bool, PlatformError> {
        #[cfg(windows)]
        {
            input_desktop_available()?;
            Ok(![0x11, 0x12, 0x10, 0x5b, 0x5c].into_iter().any(key_down))
        }
        #[cfg(not(windows))]
        {
            // There is no portable way to ask whether a modifier is held, and
            // the platforms below do not need one: the shortcut is a chord,
            // so by the time transcription finishes it has been released.
            Ok(true)
        }
    }

    fn foreground_window(&mut self) -> Option<WindowTarget> {
        #[cfg(windows)]
        {
            let hwnd = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
            return (!hwnd.is_null()).then_some(WindowTarget(hwnd as isize));
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            // X11 only; on Wayland xdotool cannot see the compositor's
            // windows, and paste falls back to plain Ctrl+V.
            let output = std::process::Command::new("xdotool")
                .arg("getactivewindow")
                .output()
                .ok()?;
            if !output.status.success() {
                return None;
            }
            let id = String::from_utf8_lossy(&output.stdout).trim().parse().ok()?;
            return Some(WindowTarget(id));
        }
        #[cfg(target_os = "macos")]
        {
            // The overlay is shown without focus, so whatever the user was
            // writing into is still the frontmost application.
            None
        }
    }

    fn restore_foreground(&mut self, target: WindowTarget) -> Result<(), PlatformError> {
        #[cfg(windows)]
        {
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, IsIconic,
                SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow,
            };
            use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};

            let hwnd = target.0 as windows_sys::Win32::Foundation::HWND;
            if hwnd.is_null() {
                return Ok(());
            }

            unsafe {
                // Only call SW_RESTORE if the window is truly minimized (iconic).
                // Calling SW_RESTORE on a maximized or snapped/split-screen window
                // causes Windows to unsnap/unmaximize it and reset its size and position!
                if IsIconic(hwnd) != 0 {
                    ShowWindow(hwnd, SW_RESTORE);
                } else {
                    ShowWindow(hwnd, SW_SHOW);
                }

                let current_thread = GetCurrentThreadId();
                let target_thread = GetWindowThreadProcessId(hwnd, std::ptr::null_mut());
                if current_thread != target_thread && target_thread != 0 {
                    AttachThreadInput(current_thread, target_thread, 1);
                    let success = SetForegroundWindow(hwnd);
                    BringWindowToTop(hwnd);
                    AttachThreadInput(current_thread, target_thread, 0);
                    if success == 0 {
                        return Err(PlatformError::FocusFailed(
                            "Windows refused to restore the target foreground window".into(),
                        ));
                    }
                } else {
                    if SetForegroundWindow(hwnd) == 0 {
                        return Err(PlatformError::FocusFailed(
                            "Windows refused to restore the target foreground window".into(),
                        ));
                    }
                    BringWindowToTop(hwnd);
                }
            }

            // Wait for Windows and target process (especially Electron/Chromium apps like Termius)
            // to switch active foreground context and focus the input element
            for _ in 0..10 {
                let fg = unsafe { GetForegroundWindow() };
                if fg == hwnd {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
            if unsafe { GetForegroundWindow() } != hwnd {
                return Err(PlatformError::FocusFailed("target did not become foreground; text is in the clipboard".into()));
            }
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let id = target.0.to_string();
            let output = std::process::Command::new("xdotool")
                .args(["windowactivate", "--sync", id.as_str()])
                .output()
                .map_err(|error| PlatformError::FocusFailed(error.to_string()))?;
            if !output.status.success() {
                return Err(PlatformError::FocusFailed(
                    String::from_utf8_lossy(&output.stderr).into_owned(),
                ));
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
            use windows_sys::Win32::UI::Input::KeyboardAndMouse::{KEYEVENTF_KEYUP, VK_CONTROL};
            let down = [key_input(VK_CONTROL, 0), key_input(u16::from(b'V'), 0)];
            let up = [
                key_input(u16::from(b'V'), KEYEVENTF_KEYUP),
                key_input(VK_CONTROL, KEYEVENTF_KEYUP),
            ];
            send_inputs_with_hold(&down, &up)
        }
        #[cfg(target_os = "macos")]
        {
            macos_keystroke(r#"tell application "System Events" to keystroke "v" using command down"#)
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            xdotool_key("ctrl+v")
        }
        #[cfg(not(any(windows, target_os = "macos", all(unix, not(target_os = "macos")))))]
        {
            Err(PlatformError::PasteFailed(
                "paste injection is not implemented on this platform".into(),
            ))
        }
    }

    fn send_ctrl_shift_v(&mut self) -> Result<(), PlatformError> {
        #[cfg(windows)]
        {
            use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
                KEYEVENTF_KEYUP, VK_CONTROL, VK_SHIFT,
            };
            let down = [
                key_input(VK_CONTROL, 0),
                key_input(VK_SHIFT, 0),
                key_input(u16::from(b'V'), 0),
            ];
            let up = [
                key_input(u16::from(b'V'), KEYEVENTF_KEYUP),
                key_input(VK_SHIFT, KEYEVENTF_KEYUP),
                key_input(VK_CONTROL, KEYEVENTF_KEYUP),
            ];
            send_inputs_with_hold(&down, &up)
        }
        #[cfg(target_os = "macos")]
        {
            macos_keystroke(
                r#"tell application "System Events" to keystroke "v" using {command down, shift down}"#,
            )
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            xdotool_key("ctrl+shift+v")
        }
        #[cfg(not(any(windows, target_os = "macos", all(unix, not(target_os = "macos")))))]
        {
            Err(PlatformError::PasteFailed(
                "paste injection is not implemented on this platform".into(),
            ))
        }
    }

    fn send_shift_insert(&mut self) -> Result<(), PlatformError> {
        #[cfg(windows)]
        {
            use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
                KEYEVENTF_KEYUP, VK_INSERT, VK_SHIFT,
            };
            let down = [key_input(VK_SHIFT, 0), key_input(VK_INSERT, 0)];
            let up = [
                key_input(VK_INSERT, KEYEVENTF_KEYUP),
                key_input(VK_SHIFT, KEYEVENTF_KEYUP),
            ];
            send_inputs_with_hold(&down, &up)
        }
        #[cfg(target_os = "macos")]
        {
            self.send_ctrl_v()
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            xdotool_key("Shift+Insert")
        }
        #[cfg(not(any(windows, target_os = "macos", all(unix, not(target_os = "macos")))))]
        {
            Err(PlatformError::PasteFailed(
                "paste injection is not implemented on this platform".into(),
            ))
        }
    }

    fn send_paste(&mut self, target: Option<WindowTarget>, mode: PasteMode) -> Result<(), PlatformError> {
        match mode {
            PasteMode::CtrlV => self.send_ctrl_v(),
            PasteMode::CtrlShiftV => self.send_ctrl_shift_v(),
            PasteMode::ShiftInsert => self.send_shift_insert(),
            PasteMode::Auto => {
                #[cfg(windows)]
                {
                    if let Some(target) = target {
                        let hwnd = target.0 as windows_sys::Win32::Foundation::HWND;
                        match detect_terminal_paste_type(hwnd) {
                            TerminalPasteType::CtrlShiftV => return self.send_ctrl_shift_v(),
                            TerminalPasteType::ShiftInsert => return self.send_shift_insert(),
                            TerminalPasteType::None => return self.send_ctrl_v(),
                        }
                    }
                    self.send_ctrl_v()
                }
                #[cfg(all(unix, not(target_os = "macos")))]
                {
                    // Terminals use Ctrl+Shift+V, everything else Ctrl+V.
                    // Without xdotool we cannot tell, and Ctrl+V is the choice
                    // that does the right thing in the majority of windows.
                    if xdotool_terminal_active() {
                        self.send_ctrl_shift_v().or_else(|_| self.send_ctrl_v())
                    } else {
                        self.send_ctrl_v()
                    }
                }
                #[cfg(target_os = "macos")]
                {
                    // macOS terminals use the same Command+V as everything
                    // else, so there is nothing to detect.
                    self.send_ctrl_v()
                }
                #[cfg(not(any(windows, target_os = "macos", all(unix, not(target_os = "macos")))))]
                {
                    Err(PlatformError::PasteFailed(
                        "paste injection is not implemented on this platform".into(),
                    ))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    /// Explorer parses its own command line, so the argument has to be
    /// exactly what it expects: a bare quoted path opens a folder, `/select,`
    /// reveals a file, and the verbatim prefix `canonicalize` adds means
    /// nothing to it — left in, it silently opens Documents instead.
    #[cfg(windows)]
    #[test]
    fn explorer_gets_an_argument_it_can_actually_parse() {
        use std::path::Path;

        assert_eq!(
            super::explorer_argument(Path::new(r"\\?\C:\Users\a\models"), true),
            r#""C:\Users\a\models""#
        );
        assert_eq!(
            super::explorer_argument(Path::new(r"\\?\C:\Users\a\clip.wav"), false),
            r#"/select,"C:\Users\a\clip.wav""#
        );
        // A path that was never canonicalised is passed through unharmed.
        assert_eq!(
            super::explorer_argument(Path::new(r"D:\sounds"), true),
            r#""D:\sounds""#
        );
    }

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
        ctrl_shift_v_attempts: usize,
        shift_insert_attempts: usize,
        fail_clipboard: bool,
        fail_focus: bool,
        fail_paste: bool,
        #[cfg(windows)]
        terminal_paste: Option<TerminalPasteType>,
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

        fn send_ctrl_shift_v(&mut self) -> Result<(), PlatformError> {
            self.ctrl_shift_v_attempts += 1;
            if self.fail_paste {
                return Err(PlatformError::PasteFailed("SendInput".into()));
            }
            Ok(())
        }

        fn send_shift_insert(&mut self) -> Result<(), PlatformError> {
            self.shift_insert_attempts += 1;
            if self.fail_paste {
                return Err(PlatformError::PasteFailed("SendInput".into()));
            }
            Ok(())
        }

        fn send_paste(&mut self, target: Option<WindowTarget>, mode: PasteMode) -> Result<(), PlatformError> {
            match mode {
                PasteMode::CtrlV => self.send_ctrl_v(),
                PasteMode::CtrlShiftV => self.send_ctrl_shift_v(),
                PasteMode::ShiftInsert => self.send_shift_insert(),
                PasteMode::Auto => {
                    #[cfg(windows)]
                    if let Some(paste_type) = self.terminal_paste {
                        return match paste_type {
                            TerminalPasteType::CtrlShiftV => self.send_ctrl_shift_v(),
                            TerminalPasteType::ShiftInsert => self.send_shift_insert(),
                            TerminalPasteType::None => self.send_ctrl_v(),
                        };
                    }
                    let _ = target;
                    self.send_ctrl_v()
                }
            }
        }
    }

    #[test]
    fn clipboard_success_and_paste_failure_are_distinct() {
        let mut windows = FakeWindows {
            fail_paste: true,
            ..Default::default()
        };

        let result = copy_and_paste(&mut windows, Some(WindowTarget(9)), "Zażółć", PasteMode::Auto);

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

        let result = copy_and_paste(&mut windows, Some(WindowTarget(9)), "tekst", PasteMode::Auto);

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
            shortcut_role("Enter", "Ctrl+Escape").unwrap(),
            ShortcutRole::ConfirmCancel
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
    fn remembered_overlay_position_is_clamped_to_the_visible_displays() {
        let work = PhysicalRect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        let dual = PhysicalRect {
            left: 0,
            top: 0,
            right: 3840,
            bottom: 1080,
        };
        let size = PhysicalWindowSize {
            width: 68,
            height: 36,
        };

        assert_eq!(
            overlay_origin(work, dual, size, Some((-400, 5000))),
            (0, 1044)
        );
        assert_eq!(
            overlay_origin(work, dual, size, Some((2500, 800))),
            (2500, 800)
        );
        assert_eq!(
            overlay_origin(work, work, size, Some((3000, -20))),
            (1852, 0)
        );
        assert_eq!(
            overlay_origin(work, dual, size, None),
            bottom_center_position(work, size)
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
            CoordinatorAction::Cancel
        );
    }

    #[test]
    fn terminal_paste_routes_to_ctrl_shift_v_or_shift_insert() {
        #[cfg(windows)]
        {
            let mut terminal_windows = FakeWindows {
                terminal_paste: Some(TerminalPasteType::CtrlShiftV),
                ..Default::default()
            };
            let _ = copy_and_paste(&mut terminal_windows, Some(WindowTarget(10)), "ls -la", PasteMode::Auto);
            assert_eq!(terminal_windows.ctrl_shift_v_attempts, 1);
            assert_eq!(terminal_windows.paste_attempts, 0);
            assert_eq!(terminal_windows.shift_insert_attempts, 0);

            let mut putty_windows = FakeWindows {
                terminal_paste: Some(TerminalPasteType::ShiftInsert),
                ..Default::default()
            };
            let _ = copy_and_paste(&mut putty_windows, Some(WindowTarget(11)), "top", PasteMode::Auto);
            assert_eq!(putty_windows.shift_insert_attempts, 1);
            assert_eq!(putty_windows.paste_attempts, 0);
            assert_eq!(putty_windows.ctrl_shift_v_attempts, 0);

            let mut explicit_shift_insert = FakeWindows::default();
            let _ = copy_and_paste(&mut explicit_shift_insert, Some(WindowTarget(12)), "echo hi", PasteMode::ShiftInsert);
            assert_eq!(explicit_shift_insert.shift_insert_attempts, 1);

            let mut explicit_ctrl_shift_v = FakeWindows::default();
            let _ = copy_and_paste(&mut explicit_ctrl_shift_v, Some(WindowTarget(13)), "echo hi", PasteMode::CtrlShiftV);
            assert_eq!(explicit_ctrl_shift_v.ctrl_shift_v_attempts, 1);
        }
    }
}


/// Explicit interactive Windows smoke: only two windows created by this test
/// receive focus/input. Not part of the unattended test suite.
#[cfg(all(test, windows))]
mod native_scratch_tests {
    use super::*;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, DispatchMessageW, ES_AUTOVSCROLL, ES_MULTILINE,
        GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW, MSG, PM_REMOVE,
        PeekMessageW, TranslateMessage, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };

    struct ScratchWindow(HWND);
    impl ScratchWindow {
        fn new(x: i32, y: i32) -> Self {
            let class: Vec<u16> = "EDIT\0".encode_utf16().collect();
            let empty = [0u16];
            let hwnd = unsafe {
                CreateWindowExW(
                    0, class.as_ptr(), empty.as_ptr(),
                    WS_OVERLAPPEDWINDOW | WS_VISIBLE | ES_MULTILINE as u32 | ES_AUTOVSCROLL as u32,
                    x, y, 640, 180, std::ptr::null_mut(), std::ptr::null_mut(),
                    std::ptr::null_mut(), std::ptr::null(),
                )
            };
            assert!(!hwnd.is_null(), "could not create the owned scratch edit window");
            Self(hwnd)
        }
        fn text(&self) -> String {
            let length = unsafe { GetWindowTextLengthW(self.0) };
            let mut text = vec![0u16; length.max(0) as usize + 1];
            let copied = unsafe { GetWindowTextW(self.0, text.as_mut_ptr(), text.len() as i32) };
            String::from_utf16(&text[..copied.max(0) as usize]).unwrap()
        }
        fn target(&self) -> WindowTarget { WindowTarget(self.0 as isize) }
        fn focus(&self, windows: &mut SystemWindows) {
            windows.restore_foreground(self.target()).expect("scratch window must become foreground");
            unsafe { SetFocus(self.0); }
            pump(Duration::from_millis(40));
            assert_eq!(unsafe { GetForegroundWindow() }, self.0);
        }
    }
    impl Drop for ScratchWindow {
        fn drop(&mut self) { unsafe { DestroyWindow(self.0); } }
    }

    /// Restore text only if the test's last write still owns the clipboard.
    /// An external copy wins, including one containing the same text.
    struct ClipboardRestore {
        original: String,
        owner: HWND,
        owned_sequence: Option<u32>,
        owned_text: String,
    }
    impl ClipboardRestore {
        fn new(original: String, owner: HWND) -> Self {
            Self { original, owner, owned_sequence: None, owned_text: String::new() }
        }
        fn record_write(&mut self, text: &str) {
            let sequence = unsafe { GetClipboardSequenceNumber() };
            if arboard::Clipboard::new().ok().and_then(|mut clipboard| clipboard.get_text().ok()).as_deref() == Some(text) {
                self.owned_sequence = Some(sequence);
                self.owned_text = text.to_owned();
            }
        }
    }
    impl Drop for ClipboardRestore {
        fn drop(&mut self) {
            use windows_sys::Win32::Foundation::GlobalFree;
            use windows_sys::Win32::System::DataExchange::{OpenClipboard, CloseClipboard, EmptyClipboard, SetClipboardData};
            use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
            use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
            // Hold the native clipboard lock across the ownership check and
            // restore. An external copy cannot race between check and write.
            unsafe {
                if OpenClipboard(self.owner) == 0 { return; }
                if self.owned_sequence == Some(GetClipboardSequenceNumber()) {
                    let utf16: Vec<u16> = self.original.encode_utf16().chain(Some(0)).collect();
                    let allocation = GlobalAlloc(GMEM_MOVEABLE, utf16.len() * 2);
                    if !allocation.is_null() {
                        let destination = GlobalLock(allocation) as *mut u16;
                        if !destination.is_null() {
                            std::ptr::copy_nonoverlapping(utf16.as_ptr(), destination, utf16.len());
                            GlobalUnlock(allocation);
                            if EmptyClipboard() == 0 || SetClipboardData(CF_UNICODETEXT as u32, allocation).is_null() {
                                GlobalFree(allocation);
                            }
                        } else { GlobalFree(allocation); }
                    }
                }
                CloseClipboard();
            }
        }
    }

    fn pump(duration: Duration) {
        let deadline = Instant::now() + duration;
        loop {
            let mut message = MSG::default();
            while unsafe { PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
                unsafe { TranslateMessage(&message); DispatchMessageW(&message); }
            }
            if Instant::now() >= deadline { break; }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    #[ignore = "interactive Windows scratch-window clipboard/focus/paste smoke; run explicitly on an idle desktop"]
    fn native_draft_is_clipboard_only_and_final_pastes_once_into_owned_scratch() {
        // Capture before creating any window or changing clipboard contents.
        let original = arboard::Clipboard::new().unwrap().get_text()
            .expect("native smoke requires a text clipboard so it can restore it exactly");
        let target = ScratchWindow::new(60, 60);
        let decoy = ScratchWindow::new(100, 280);
        // Declared after the windows so restoration also precedes their
        // destruction when an assertion unwinds.
        let mut restore = ClipboardRestore::new(original, target.0);
        let mut windows = SystemWindows;
        target.focus(&mut windows);

        for draft in ["Pierwszy szkic", "Ca\u{0142}kowicie poprawiony szkic \u{1f99c}"] {
            windows.write_clipboard(draft).unwrap();
            restore.record_write(draft);
            pump(Duration::from_millis(120));
            assert_eq!(target.text(), "", "draft must never type into the target");
            assert_eq!(decoy.text(), "", "draft must never type elsewhere");
            assert_eq!(arboard::Clipboard::new().unwrap().get_text().unwrap(), draft);
        }

        // A different owned window is focused, proving that delivery restores
        // the captured target instead of sending to whichever window is active.
        decoy.focus(&mut windows);
        let final_text = "Za\u{017c}\u{00f3}\u{0142}\u{0107} g\u{0119}\u{015b}l\u{0105} ja\u{017a}\u{0144}. Podsumowanie spotkania jutro rano. \u{1f99c}";
        let delivery = copy_and_paste(&mut windows, Some(target.target()), final_text, PasteMode::Auto);
        restore.record_write(final_text);
        delivery.expect("native final paste into the owned scratch window must succeed");
        assert_eq!(unsafe { GetForegroundWindow() }, target.0, "final must restore the captured target");
        pump(Duration::from_millis(700));
        assert_eq!(target.text(), final_text, "final must appear exactly once, with all Unicode intact");
        assert_eq!(decoy.text(), "", "focus changes must not redirect the final to another window");
        assert_eq!(arboard::Clipboard::new().unwrap().get_text().unwrap(), final_text);
        pump(Duration::from_millis(300));
        assert_eq!(target.text(), final_text, "no delayed second paste may arrive");
    }
}

#[cfg(test)]
mod ptt_modifier_tests {
    use super::*;
    struct Held { polls: usize, sleeps: usize, unavailable: bool }
    impl WindowsApi for Held {
        fn foreground_window(&mut self) -> Option<WindowTarget> { None }
        fn restore_foreground(&mut self, _: WindowTarget) -> Result<(), PlatformError> { panic!("no focus during wait") }
        fn write_clipboard(&mut self, _: &str) -> Result<(), PlatformError> { panic!("no clipboard during wait") }
        fn send_ctrl_v(&mut self) -> Result<(), PlatformError> { panic!("no input during wait") }
        fn modifiers_released(&mut self) -> Result<bool, PlatformError> {
            if self.unavailable { return Err(PlatformError::PasteFailed("desktop unavailable".into())); }
            self.polls += 1; Ok(self.polls >= 3)
        }
        fn pause_for_modifiers(&mut self) { self.sleeps += 1; }
    }
    #[test]
    fn ptt_native_chord_mapping_observes_each_modifier_and_supported_main_key() {
        let chord = Shortcut::parse("Ctrl+Alt+Shift+Win+F12").unwrap();
        let required = [0x11, 0x12, 0x10, 0x5b, 0x7b];
        assert!(chord.sample_keys(|key| required.contains(&key)).chord_down);
        for released in required {
            let keys = chord.sample_keys(|key| key != released && required.contains(&key));
            assert!(!keys.chord_down);
            assert_eq!(keys.main_down, released != 0x7b);
        }
        assert!(Shortcut::parse("Ctrl+Win").is_err());
        assert!(Shortcut::parse("CapsLock").is_err());
        for (binding, main) in [("Ctrl+Space", 0x20), ("Alt+Enter", 0x0d), ("Ctrl+A", 0x41), ("Ctrl+5", 0x35), ("Ctrl+F24", 0x87)] {
            assert!(Shortcut::parse(binding).unwrap().sample_keys(|key| [0x11, 0x12, main].contains(&key)).chord_down);
        }
    }

    #[test]
    fn ptt_modifier_wait_requires_release_and_is_bounded_and_cancellable() {
        let mut target = Held { polls: 0, sleeps: 0, unavailable: false };
        assert!(wait_for_modifiers(&mut target, None, || false, || true, std::time::Duration::from_secs(1)).is_ok());
        assert_eq!(target.polls, 3); assert_eq!(target.sleeps, 2);
        target.polls = 0;
        assert!(wait_for_modifiers(&mut target, None, || false, || true, std::time::Duration::ZERO).is_err());
        assert!(wait_for_modifiers(&mut target, None, || true, || true, std::time::Duration::from_secs(1)).is_err());
        target.unavailable = true;
        assert!(wait_for_modifiers(&mut target, None, || false, || true, std::time::Duration::from_secs(1)).is_err());
    }
}
