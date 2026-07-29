pub mod audio;
pub mod dictation;
pub mod domain;
pub mod platform;
pub mod storage;
pub mod transcription;

use crate::dictation::AppState;
use std::path::{Path, PathBuf};
use tauri::menu::MenuBuilder;
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, WindowEvent};
use tauri_plugin_global_shortcut::ShortcutState;

fn worker_path_candidates(current: &Path, resources: Option<&Path>) -> Vec<PathBuf> {
    let mut candidates = vec![
        current.join("engine").join("parakeet_worker.py"),
        current
            .parent()
            .unwrap_or(current)
            .join("engine")
            .join("parakeet_worker.py"),
    ];
    if let Some(resources) = resources {
        candidates.push(resources.join("engine").join("parakeet_worker.py"));
        candidates.push(
            resources
                .join("_up_")
                .join("engine")
                .join("parakeet_worker.py"),
        );
    }
    candidates
}

fn select_worker_path(
    candidates: &[PathBuf],
    mut is_file: impl FnMut(&Path) -> bool,
) -> Option<PathBuf> {
    candidates.iter().find(|path| is_file(path)).cloned()
}

fn resolve_worker_path(
    current: &Path,
    resources: Option<&Path>,
) -> Result<PathBuf, std::io::Error> {
    let candidates = worker_path_candidates(current, resources);
    select_worker_path(&candidates, Path::is_file).ok_or_else(|| {
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
                            let _ = dictation::cancel_recording_inner(&app, &state);
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
            let worker_path = resolve_worker_path(&current_dir, Some(&resource_dir))?;
            let python = transcription::resolve_python_executable()?;
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let recordings_dir = data_dir.join("recordings");
            let storage = storage::Storage::open(data_dir.join("mow.sqlite3"), &recordings_dir)
                .map_err(|error| error.to_string())?;
            let state = AppState::new(
                audio::AudioRecorder::new(recordings_dir),
                storage,
                python,
                worker_path,
            );
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
            app.manage(state);
            platform::register_shortcuts(app.handle(), &shortcut)
                .map_err(|error| error.to_string())?;

            let menu = MenuBuilder::new(app)
                .text("toggle", "Start/Stop recording")
                .text("paste", "Paste last transcript")
                .separator()
                .text("open", "Open Mów")
                .text("quit", "Quit")
                .build()?;
            let mut tray = TrayIconBuilder::with_id("mow")
                .menu(&menu)
                .tooltip("Mów")
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
            }
            tray.build(app)?;
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
            dictation::retry_transcription,
            dictation::paste_transcript,
            dictation::list_history,
            dictation::delete_history,
            dictation::list_vocabulary,
            dictation::add_vocabulary,
            dictation::delete_vocabulary,
            dictation::list_modes,
            dictation::upsert_mode,
            dictation::delete_mode,
            dictation::get_settings,
            dictation::get_model_status,
            dictation::update_settings,
            dictation::update_setting_value,
        ])
        .run(tauri::generate_context!())
        .expect("nie udało się uruchomić aplikacji Mów");
}

#[cfg(test)]
mod packaging_tests {
    use super::*;

    #[test]
    fn worker_candidates_prefer_dev_repo_then_bundled_resource_layouts() {
        let current = PathBuf::from(r"C:\kod\mów");
        let resources = PathBuf::from(r"C:\Program Files\Mów");

        assert_eq!(
            worker_path_candidates(&current, Some(&resources)),
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

        let selected = select_worker_path(&candidates, |path| path == candidates[1].as_path());

        assert_eq!(selected, Some(candidates[1].clone()));
    }
}
