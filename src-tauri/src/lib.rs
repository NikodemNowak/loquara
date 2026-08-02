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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkerResolutionMode {
    Debug,
    Release,
}

fn worker_path_candidates(
    current: &Path,
    resources: &Path,
    mode: WorkerResolutionMode,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if mode == WorkerResolutionMode::Debug {
        candidates.extend([
            current.join("engine").join("parakeet_worker.py"),
            current
                .parent()
                .unwrap_or(current)
                .join("engine")
                .join("parakeet_worker.py"),
        ]);
    }
    candidates.push(resources.join("engine").join("parakeet_worker.py"));
    candidates.push(
        resources
            .join("_up_")
            .join("engine")
            .join("parakeet_worker.py"),
    );
    candidates
}

fn select_worker_path(
    candidates: &[PathBuf],
    trusted_root: Option<&Path>,
    mut canonicalize: impl FnMut(&Path) -> std::io::Result<PathBuf>,
    mut is_file: impl FnMut(&Path) -> bool,
) -> Option<PathBuf> {
    let trusted_root = trusted_root.and_then(|path| canonicalize(path).ok());
    candidates.iter().find_map(|candidate| {
        let canonical = canonicalize(candidate).ok()?;
        if trusted_root
            .as_ref()
            .is_some_and(|root| !canonical.starts_with(root))
            || !is_file(&canonical)
        {
            return None;
        }
        Some(canonical)
    })
}

fn resolve_worker_path(
    current: &Path,
    resources: &Path,
    mode: WorkerResolutionMode,
) -> Result<PathBuf, std::io::Error> {
    let candidates = worker_path_candidates(current, resources, mode);
    let trusted_root = (mode == WorkerResolutionMode::Release).then_some(resources);
    select_worker_path(
        &candidates,
        trusted_root,
        |path| std::fs::canonicalize(path),
        |path| path.is_file(),
    )
    .ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "nie znaleziono workera Parakeet; sprawdzono: {}",
                candidates
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
    })
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct HealthStatus {
    pid: u32,
    tray_initialized: bool,
    identifier: String,
    version: String,
    resource_worker_exists: bool,
}

fn health_path(data_dir: &Path) -> PathBuf {
    data_dir.join("health.json")
}

fn health_payload(
    pid: u32,
    identifier: &str,
    version: &str,
    resource_worker_exists: bool,
) -> HealthStatus {
    HealthStatus {
        pid,
        tray_initialized: true,
        identifier: identifier.to_owned(),
        version: version.to_owned(),
        resource_worker_exists,
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
                            let _ = dictation::request_cancel_inner(&app, &state);
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
            let current_dir = std::env::current_dir()?;
            let resource_dir = app.path().resource_dir()?;
            let resolution_mode = if cfg!(debug_assertions) {
                WorkerResolutionMode::Debug
            } else {
                WorkerResolutionMode::Release
            };
            let worker_path =
                resolve_worker_path(&current_dir, &resource_dir, resolution_mode)?;
            let resource_worker_exists = worker_path.is_file();
            // Keep the UI available when Python is missing; dictation will
            // report the actionable runtime error instead of aborting startup.
            let python = transcription::resolve_python_executable()
                .unwrap_or_else(|_| transcription::PythonExecutable::new("python", Vec::<&str>::new()));
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let health_marker = health_path(&data_dir);
            clear_health_marker(&health_marker)?;
            let recordings_dir = data_dir.join("recordings");
            let storage = storage::Storage::open(data_dir.join("loquara.sqlite3"), &recordings_dir)
                .map_err(|error| error.to_string())?;
            let worker_log = data_dir.join("worker.log");
            let state = AppState::new(
                audio::AudioRecorder::new(recordings_dir),
                storage,
                python,
                worker_path,
                worker_log,
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
            let health = health_payload(
                std::process::id(),
                &app.config().identifier,
                &app.package_info().version.to_string(),
                resource_worker_exists,
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
            dictation::retry_transcription,
            dictation::paste_transcript,
            dictation::list_history,
            dictation::delete_history,
            dictation::play_recording,
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
            dictation::delete_model,
            dictation::update_settings,
            dictation::update_setting_value,
            dictation::system_locale,
        ])
        .run(tauri::generate_context!())
        .expect("nie udało się uruchomić aplikacji Loquara");
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
            mic_none: "(brak mikrofonów)",
            open: "Otwórz Loquarę",
            quit: "Zakończ",
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

#[cfg(test)]
mod packaging_tests {
    use super::*;

    #[test]
    fn worker_candidates_prefer_dev_repo_then_bundled_resource_layouts() {
        let current = PathBuf::from(r"C:\kod\mów");
        let resources = PathBuf::from(r"C:\Program Files\Mów");

        assert_eq!(
            worker_path_candidates(
                &current,
                &resources,
                WorkerResolutionMode::Debug,
            ),
            vec![
                current.join("engine").join("parakeet_worker.py"),
                current
                    .parent()
                    .unwrap()
                    .join("engine")
                    .join("parakeet_worker.py"),
                resources.join("engine").join("parakeet_worker.py"),
                resources
                    .join("_up_")
                    .join("engine")
                    .join("parakeet_worker.py"),
            ]
        );
    }

    #[test]
    fn worker_selection_handles_unicode_and_uses_first_existing_candidate() {
        let candidates = vec![
            PathBuf::from(r"C:\źródła\Mów\engine\parakeet_worker.py"),
            PathBuf::from(r"C:\Program Files\Mów\_up_\engine\parakeet_worker.py"),
        ];

        let selected = select_worker_path(
            &candidates,
            None,
            |path| Ok(path.to_owned()),
            |path| path == candidates[1].as_path(),
        );

        assert_eq!(selected, Some(candidates[1].clone()));
    }

    #[test]
    fn release_worker_candidates_ignore_malicious_current_directory() {
        let current = PathBuf::from(r"C:\attacker\engine");
        let resources = PathBuf::from(r"C:\Program Files\Mów");

        let candidates = worker_path_candidates(
            &current,
            &resources,
            WorkerResolutionMode::Release,
        );

        assert_eq!(
            candidates,
            vec![
                resources.join("engine").join("parakeet_worker.py"),
                resources
                    .join("_up_")
                    .join("engine")
                    .join("parakeet_worker.py"),
            ]
        );
        assert!(candidates.iter().all(|path| !path.starts_with(&current)));
    }

    #[test]
    fn release_worker_rejects_canonical_path_outside_resource_root() {
        let resources = PathBuf::from(r"C:\Program Files\Mów");
        let candidate = resources.join("engine").join("parakeet_worker.py");
        let candidates = vec![candidate.clone()];

        let selected = select_worker_path(
            &candidates,
            Some(&resources),
            |path| {
                if path == resources {
                    Ok(resources.clone())
                } else {
                    Ok(PathBuf::from(r"C:\attacker\parakeet_worker.py"))
                }
            },
            |_| true,
        );

        assert_eq!(selected, None);
    }

    #[test]
    fn health_contract_uses_app_data_and_explicit_tray_evidence() {
        let data_dir = PathBuf::from(r"C:\Users\Ada\AppData\Roaming\pl.mow.desktop");

        assert_eq!(health_path(&data_dir), data_dir.join("health.json"));
        assert_eq!(
            health_payload(42, "pl.mow.desktop", "0.1.0", true),
            HealthStatus {
                pid: 42,
                tray_initialized: true,
                identifier: "pl.mow.desktop".into(),
                version: "0.1.0".into(),
                resource_worker_exists: true,
            }
        );
    }

    #[test]
    fn resolved_language_maps_choice_with_stubbed_system_language() {
        let system_en = || "en";
        let system_pl = || "pl";

        let pl = AppSettings {
            language: LanguageChoice::Pl,
            ..Default::default()
        };
        assert_eq!(resolve_language_with(&pl, system_en), "pl");

        let en = AppSettings {
            language: LanguageChoice::En,
            ..Default::default()
        };
        assert_eq!(resolve_language_with(&en, system_pl), "en");

        let system = AppSettings {
            language: LanguageChoice::System,
            ..Default::default()
        };
        assert_eq!(resolve_language_with(&system, system_pl), "pl");
        assert_eq!(resolve_language_with(&system, system_en), "en");
    }
}
