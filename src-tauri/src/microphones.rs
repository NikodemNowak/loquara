use crate::audio::InputDeviceInfo;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Selection { #[default] Manual, Favorites }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Favorite { pub id: String, pub name: String }

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MicrophonePreferences {
    pub selection: Selection,
    pub selected_id: Option<String>,
    pub favorites: Vec<Favorite>,
    pub hidden_ids: Vec<String>,
    pub allow_system_default_fallback: bool,
    pub unresolved_legacy_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PreferenceError {
    #[error("Microphone name is ambiguous. Choose a microphone in Settings.")]
    Ambiguous,
    #[error("Microphone is unavailable. Choose a microphone in Settings.")]
    Unavailable,
    #[error("Microphone is hidden. Restore it in Settings before recording.")]
    Hidden,
}
impl PreferenceError {
    pub fn kind(&self) -> &'static str {
        match self { Self::Ambiguous => "ambiguous", Self::Unavailable => "unavailable", Self::Hidden => "hidden" }
    }
}

pub fn migrate_legacy(name: Option<&str>, devices: &[InputDeviceInfo]) -> MicrophonePreferences {
    let mut result = MicrophonePreferences::default();
    if let Some(name) = name {
        let matches: Vec<_> = devices.iter().filter(|device| device.available && device.name == name).collect();
        if matches.len() == 1 { result.selected_id = Some(matches[0].id.clone()); }
        else { result.unresolved_legacy_name = Some(name.to_owned()); }
    }
    result
}

pub fn resolve_preference<'a>(preferences: &MicrophonePreferences, devices: &'a [InputDeviceInfo]) -> Result<&'a InputDeviceInfo, PreferenceError> {
    if let Some(name) = &preferences.unresolved_legacy_name {
        return Err(if devices.iter().filter(|device| device.available && &device.name == name).count() > 1 {
            PreferenceError::Ambiguous
        } else { PreferenceError::Unavailable });
    }
    let visible = |device: &&InputDeviceInfo| device.available && !preferences.hidden_ids.contains(&device.id);
    match preferences.selection {
        Selection::Manual => {
            let device = devices.iter().find(|device| device.available && match &preferences.selected_id {
                Some(id) => &device.id == id, None => device.is_default,
            }).ok_or(PreferenceError::Unavailable)?;
            if !visible(&device) { return Err(PreferenceError::Hidden); }
            Ok(device)
        }
        Selection::Favorites => {
            for favorite in &preferences.favorites {
                if let Some(device) = devices.iter().filter(visible).find(|device| device.id == favorite.id) { return Ok(device); }
            }
            if preferences.allow_system_default_fallback {
                return devices.iter().filter(visible).find(|device| device.is_default).ok_or(PreferenceError::Unavailable);
            }
            Err(PreferenceError::Unavailable)
        }
    }
}

pub fn device_choices(preferences: &MicrophonePreferences, mut devices: Vec<InputDeviceInfo>) -> Vec<InputDeviceInfo> {
    for favorite in &preferences.favorites {
        if !devices.iter().any(|device| device.id == favorite.id) {
            devices.push(InputDeviceInfo { id: favorite.id.clone(), name: favorite.name.clone(), available: false, is_default: false });
        }
    }
    for id in preferences.selected_id.iter().chain(preferences.hidden_ids.iter()) {
        if !devices.iter().any(|device| &device.id == id) {
            devices.push(InputDeviceInfo { id: id.clone(), name: id.clone(), available: false, is_default: false });
        }
    }
    devices
}

pub fn validate(preferences: &MicrophonePreferences) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    if preferences.favorites.iter().any(|favorite| favorite.id.is_empty() || !seen.insert(&favorite.id)) {
        return Err("Invalid microphone favorites.".into());
    }
    if preferences.selected_id.as_ref().is_some_and(|id| id.is_empty()) || preferences.hidden_ids.iter().any(String::is_empty) {
        return Err("Invalid microphone endpoint.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn devices() -> Vec<InputDeviceInfo> {
        vec![InputDeviceInfo { id: "endpoint-a".into(), name: "Same".into(), is_default: true, available: true },
             InputDeviceInfo { id: "endpoint-b".into(), name: "Same".into(), is_default: false, available: true }]
    }
    #[test]
    fn exact_unique_name_migrates_but_duplicate_unknown_and_changed_case_do_not() {
        let mut devices = devices();
        let ambiguous = migrate_legacy(Some("Same"), &devices);
        assert_eq!(resolve_preference(&ambiguous, &devices).unwrap_err().kind(), "ambiguous");
        devices[1].name = "USB".into();
        assert_eq!(migrate_legacy(Some("USB"), &devices).selected_id.as_deref(), Some("endpoint-b"));
        for name in ["usb", "Missing"] {
            let migrated = migrate_legacy(Some(name), &devices);
            assert_eq!(migrated.unresolved_legacy_name.as_deref(), Some(name));
            assert!(resolve_preference(&migrated, &devices).is_err());
        }
        assert_eq!(resolve_preference(&migrate_legacy(None, &devices), &devices).unwrap().id, "endpoint-a");
    }
    #[test]
    fn manual_endpoint_is_exact_and_never_implicitly_uses_favorites_or_default() {
        let devices = devices();
        let mut preferences = MicrophonePreferences { selected_id: Some("endpoint-b".into()), ..Default::default() };
        assert_eq!(resolve_preference(&preferences, &devices).unwrap().id, "endpoint-b");
        preferences.selected_id = Some("lost".into());
        preferences.favorites = vec![Favorite { id: "endpoint-a".into(), name: "Same".into() }];
        assert!(resolve_preference(&preferences, &devices).is_err());
    }
    #[test]
    fn favorites_order_requires_opt_in_and_default_fallback_is_explicit() {
        let devices = devices();
        let mut preferences = MicrophonePreferences { favorites: vec![Favorite { id: "endpoint-b".into(), name: "Same".into() }, Favorite { id: "endpoint-a".into(), name: "Same".into() }], ..Default::default() };
        assert_eq!(resolve_preference(&preferences, &devices).unwrap().id, "endpoint-a");
        preferences.selection = Selection::Favorites;
        assert_eq!(resolve_preference(&preferences, &devices).unwrap().id, "endpoint-b");
        preferences.hidden_ids.push("endpoint-b".into());
        assert_eq!(resolve_preference(&preferences, &devices).unwrap().id, "endpoint-a");
        preferences.favorites.clear();
        assert!(resolve_preference(&preferences, &devices).is_err());
        preferences.allow_system_default_fallback = true;
        assert_eq!(resolve_preference(&preferences, &devices).unwrap().id, "endpoint-a");
    }
    #[test]
    fn unavailable_selected_favorite_and_hidden_endpoints_remain_editable() {
        let preferences = MicrophonePreferences { selected_id: Some("selected".into()), hidden_ids: vec!["hidden".into()], favorites: vec![Favorite { id: "favorite".into(), name: "Saved USB".into() }], ..Default::default() };
        let choices = device_choices(&preferences, Vec::new());
        assert_eq!(choices.len(), 3);
        assert!(choices.iter().all(|device| !device.available && !device.is_default));
        assert!(choices.iter().any(|device| device.name == "Saved USB"));
        assert!(resolve_preference(&preferences, &choices).is_err());
    }
}
