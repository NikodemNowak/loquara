pub mod models;
pub mod engine;
pub mod audio;
pub mod dictation;
pub mod domain;
pub mod platform;
pub mod sound;
pub mod storage;
pub mod transcription;

use crate::dictation::{AppSettings, AppState, LanguageChoice};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::menu::MenuBuilder;
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, WindowEvent};
use tauri_plugin_global_shortcut::ShortcutState;


#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct HealthStatus {
    pid: u32,
    tray_initialized: bool,
    identifier: String,
    version: String,
}

fn health_path(data_dir: &Path) -> PathBuf {
    data_dir.join("health.json")
}

fn health_payload(pid: u32, identifier: &str, version: &str) -> HealthStatus {
    HealthStatus {
        pid,
        tray_initialized: true,
        identifier: identifier.to_owned(),
        version: version.to_owned(),
    }
}

fn clear_health_marker(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn write_health_atomically(path: &Path, status: &HealthStatus) -> std::io::Result<()> {
    let temporary = path.with_extension(format!("json.{}.tmp", status.pid));
    let encoded = serde_json::to_vec(status).map_err(std::io::Error::other)?;
    std::fs::write(&temporary, encoded)?;
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            let _ = platform::show_main(app);
        }))
        .plugin(tauri_plugin_autostart::Builder::new().build())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state != ShortcutState::Pressed {
                        return;
                    }
                    let app = app.clone();
                    let state = app.state::<AppState>().inner().clone();
                    let configured = state
                        .settings
                        .read()
                        .map(|settings| settings.shortcut.clone())
                        .unwrap_or_default();
                    match platform::shortcut_role_for(shortcut, &configured) {
                        Ok(platform::ShortcutRole::Cancel) => {
                            // Off the handler thread, like the toggle below.
                            // Handling a press means changing state, and every
                            // state change claims or releases Escape — which
                            // reaches back into the shortcut registry the
                            // plugin is still holding while it calls this.
                            // Done here, that is a deadlock: the window stops
                            // repainting and the prompt never appears.
                            tauri::async_runtime::spawn(async move {
                                dictation::handle_cancel_press(
                                    &app,
                                    &state,
                                    dictation::RecorderKey::Escape,
                                );
                            });
                        }
                        Ok(platform::ShortcutRole::ConfirmCancel) => {
                            tauri::async_runtime::spawn(async move {
                                dictation::handle_cancel_press(
                                    &app,
                                    &state,
                                    dictation::RecorderKey::Enter,
                                );
                            });
                        }
                        Ok(platform::ShortcutRole::Toggle) => {
                            tauri::async_runtime::spawn(async move {
                                let _ = dictation::toggle_recording(app, state).await;
                            });
                        }
                        Ok(platform::ShortcutRole::Ignore) | Err(_) => {}
                    }
                })
                .build(),
        )
        .setup(|app| {
            let _current_dir = std::env::current_dir()?;
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let health_marker = health_path(&data_dir);
            clear_health_marker(&health_marker)?;
            let recordings_dir = data_dir.join("recordings");
            let storage = storage::Storage::open(data_dir.join("loquara.sqlite3"), &recordings_dir)
                .map_err(|error| error.to_string())?;
            // A transfer that was interrupted by a closing app leaves parts
            // behind; nothing resumes them, so nothing should keep them.
            let _ = models::sweep_parts(&data_dir.join("models"));
            let state = AppState::new(
                audio::AudioRecorder::new(recordings_dir),
                storage,
                data_dir.join("models"),
            );
            // Register state before any window can invoke a Tauri command.
            app.manage(state.clone());
            let (level_sender, level_receiver) = std::sync::mpsc::sync_channel(2);
            state.audio.set_level_sender(level_sender);
            let level_app = app.handle().clone();
            std::thread::spawn(move || {
                while let Ok(rms) = level_receiver.recv() {
                    let _ = level_app.emit("dictation://level", rms);
                }
            });
            let shortcut = state
                .settings
                .read()
                .map_err(|_| "settings lock poisoned")?
                .shortcut
                .clone();
            let launch_on_login = state
                .settings
                .read()
                .map_err(|_| "settings lock poisoned")?
                .launch_on_login;
            platform::sync_autostart(app.handle(), launch_on_login)
                .map_err(|error| error.to_string())?;
            dictation::cleanup_retention(&state)?;
            dictation::warm_up_model(app.handle(), &state);
            dictation::spawn_model_keep_alive_watchdog(&state);
            platform::register_shortcuts(app.handle(), &shortcut)
                .map_err(|error| error.to_string())?;

            let settings_guard = state
                .settings
                .read()
                .map_err(|_| "settings lock poisoned")?;
            let locale = resolved_language(&settings_guard);
            let menu = build_tray_menu(app, &state, locale)?;
            let mut tray = TrayIconBuilder::with_id("loquara")
                .menu(&menu)
                .tooltip("Loquara")
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "toggle" => {
                        let app = app.clone();
                        let state = app.state::<AppState>().inner().clone();
                        tauri::async_runtime::spawn(async move {
                            let _ = dictation::toggle_recording(app, state).await;
                        });
                    }
                    "paste" => {
                        let state = app.state::<AppState>();
                        let _ = dictation::paste_transcript_inner(None, &state);
                    }
                    "open" => {
                        let _ = platform::show_main(app);
                    }
                    "quit" => app.exit(0),
                    id if id.starts_with("mic:") => {
                        let device = id.trim_start_matches("mic:").to_owned();
                        let state = app.state::<AppState>();
                        dictation::set_input_device(&state, device);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if matches!(
                        event,
                        TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        }
                    ) {
                        let _ = platform::show_main(tray.app_handle());
                    }
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.set_icon(icon.clone());
                }
            }
            tray.build(app)?;
            // The window is configured hidden so a quiet start never flashes
            // it on screen. It is shown here, once the tray exists to bring it
            // back, unless the user asked to start in the tray.
            let start_minimized = state
                .settings
                .read()
                .map(|settings| settings.start_minimized)
                .unwrap_or(false);
            if !start_minimized {
                platform::show_main(app.handle())?;
            }
            let health = health_payload(
                std::process::id(),
                &app.config().identifier,
                &app.package_info().version.to_string(),
            );
            write_health_atomically(&health_marker, &health)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "main"
                && let WindowEvent::CloseRequested { api, .. } = event
            {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            dictation::get_app_snapshot,
            dictation::list_input_devices,
            dictation::start_recording,
            dictation::stop_recording,
            dictation::cancel_recording,
            dictation::request_cancel,
            dictation::hide_overlay,
            dictation::save_overlay_position,
            dictation::set_shortcut_suspended,
            dictation::retry_transcription,
            dictation::paste_transcript,
            dictation::list_history,
            dictation::delete_history,
            dictation::export_transcript,
            dictation::clear_failed_recordings,
            dictation::play_recording,
            dictation::read_recording_audio,
            dictation::reveal_recording,
            dictation::reveal_recordings_dir,
            dictation::reveal_model_dir,
            dictation::correct_transcript,
            dictation::list_vocabulary,
            dictation::add_vocabulary,
            dictation::delete_vocabulary,
            dictation::list_modes,
            dictation::upsert_mode,
            dictation::delete_mode,
            dictation::get_settings,
            dictation::get_model_status,
            dictation::list_models,
            dictation::download_model,
            dictation::cancel_download,
            dictation::delete_model,
            dictation::update_settings,
            dictation::update_setting_value,
            dictation::system_locale,
        ])
        .run(tauri::generate_context!())
        .expect("failed to launch the Loquara app");
}

/// Labels for the tray context menu, localized for one UI language subtag.
struct TrayLabels {
    submenu_microphones: &'static str,
    toggle: &'static str,
    paste: &'static str,
    mic_none: &'static str,
    open: &'static str,
    quit: &'static str,
}

fn tray_labels(locale: &str) -> TrayLabels {
    match locale {
        "pl" => TrayLabels {
            submenu_microphones: "Mikrofon",
            toggle: "Start/Zatrzymaj nagrywanie",
            paste: "Wklej ostatni tekst",
            mic_none: "(no microphones)",
            open: "Open Loquara",
            quit: "Quit",
        },
        _ => TrayLabels {
            submenu_microphones: "Microphone",
            toggle: "Start/Stop recording",
            paste: "Paste last transcript",
            mic_none: "(no microphones)",
            open: "Open Loquara",
            quit: "Quit",
        },
    }
}

/// Maps the `LanguageChoice` to a UI language subtag, delegating to `system`
/// when set to `System`. Kept injectable so tests can stub the OS language.
fn resolve_language_with(settings: &AppSettings, system: fn() -> &'static str) -> &'static str {
    match settings.language {
        LanguageChoice::Pl => "pl",
        LanguageChoice::En => "en",
        LanguageChoice::System => system(),
    }
}

/// Resolves the effective UI language ("pl" or "en") from the app settings,
/// falling back to the OS UI language when set to `System`.
pub fn resolved_language(settings: &AppSettings) -> &'static str {
    resolve_language_with(settings, dictation::system_lang)
}

/// Builds the tray context menu with labels for `locale` and the current set
/// of input devices, marking the active input device with a checkmark.
fn build_tray_menu<R: tauri::Runtime, M: tauri::Manager<R>>(
    app: &M,
    state: &AppState,
    locale: &str,
) -> tauri::Result<tauri::menu::Menu<R>> {
    let labels = tray_labels(locale);
    let devices = state.audio.list_devices().unwrap_or_default();
    let active_device = state
        .settings
        .read()
        .ok()
        .and_then(|settings| settings.input_device.clone());
    let mut mic_menu =
        tauri::menu::SubmenuBuilder::with_id(app, "microphones", labels.submenu_microphones);
    if devices.is_empty() {
        mic_menu = mic_menu.text("mic:none", labels.mic_none);
    }
    for device in &devices {
        let label = if active_device.as_deref() == Some(device.id.as_str()) {
            format!("{}  ✓", device.name)
        } else {
            device.name.clone()
        };
        mic_menu = mic_menu.text(format!("mic:{}", device.id), &label);
    }
    MenuBuilder::new(app)
        .text("toggle", labels.toggle)
        .text("paste", labels.paste)
        .separator()
        .item(&mic_menu.build()?)
        .separator()
        .text("open", labels.open)
        .text("quit", labels.quit)
        .build()
}

/// Rebuilds the "loquara" tray menu in place so a language change is reflected
/// live, without restarting the app.
pub fn rebuild_tray_menu(app: &tauri::AppHandle, state: &AppState, locale: &str) {
    let menu = build_tray_menu(app, state, locale).expect("failed to build tray menu");
    let tray = app
        .tray_by_id("loquara")
        .expect("loquara tray icon should still exist");
    tray.set_menu(Some(menu))
        .expect("failed to update tray menu");
}

