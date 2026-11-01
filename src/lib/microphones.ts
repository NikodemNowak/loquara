import type { AppSettings, InputDeviceInfo, MicrophonePreferences } from "./types";

export function microphonePreferences(settings: AppSettings, devices: InputDeviceInfo[]): MicrophonePreferences {
  if (settings.microphones) return settings.microphones;
  const matches = devices.filter((device) => device.available && device.name === settings.inputDevice);
  return { selection: "manual", selectedId: settings.inputDevice !== null && matches.length === 1 ? matches[0].id : null,
    favorites: [], hiddenIds: [], allowSystemDefaultFallback: false,
    unresolvedLegacyName: settings.inputDevice !== null && matches.length !== 1 ? settings.inputDevice : null };
}

export function microphoneChoices(preferences: MicrophonePreferences, devices: InputDeviceInfo[]): InputDeviceInfo[] {
  const result = [...devices];
  const saved = [...preferences.favorites, ...[preferences.selectedId, ...preferences.hiddenIds].filter((id): id is string => id !== null).map((id) => ({ id, name: id }))];
  for (const device of saved) if (!result.some((item) => item.id === device.id)) result.push({ ...device, isDefault: false, available: false });
  return result;
}

export function resolveMicrophone(preferences: MicrophonePreferences, devices: InputDeviceInfo[]): InputDeviceInfo {
  const available = devices.filter((device) => device.available && !preferences.hiddenIds.includes(device.id));
  if (preferences.unresolvedLegacyName) throw new Error("Microphone requires an explicit choice in Settings.");
  let chosen: InputDeviceInfo | undefined;
  if (preferences.selection === "manual") chosen = preferences.selectedId === null ? available.find((device) => device.isDefault) : available.find((device) => device.id === preferences.selectedId);
  else {
    for (const favorite of preferences.favorites) { chosen = available.find((device) => device.id === favorite.id); if (chosen) break; }
    if (!chosen && preferences.allowSystemDefaultFallback) chosen = available.find((device) => device.isDefault);
  }
  if (!chosen) throw new Error("Microphone unavailable. Choose a microphone in Settings.");
  return chosen;
}
