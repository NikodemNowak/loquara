import { useEffect, useState } from "react";

import { Check, Mic } from "../../components/Icons";
import { applyTheme, initialTheme } from "../../app/theme";
import type { ToastKind } from "../../components/Toast";
import type { AppAdapter } from "../../lib/tauri";
import type { AppSettings, InputDeviceInfo, ModelStatus, ThemeChoice } from "../../lib/types";
import { normalizeError } from "../../lib/errors";

const shortcutPattern = /^(?:(?:Ctrl|Alt|Shift|Meta)\+)+(?:[A-Z0-9]|Space|Enter|F(?:[1-9]|1[0-2]))$/i;

export function SettingsPage({
  adapter,
  initialSettings,
  onSettingsChange,
  onToast,
}: {
  adapter: AppAdapter;
  initialSettings: AppSettings;
  onSettingsChange?: (settings: AppSettings) => void;
  onToast: (message: string, kind: ToastKind) => void;
}) {
  const [settings, setSettings] = useState(initialSettings);
  const [devices, setDevices] = useState<InputDeviceInfo[]>([]);
  const [deviceError, setDeviceError] = useState("");
  const [modelStatus, setModelStatus] = useState<ModelStatus>();
  const [theme, setTheme] = useState<ThemeChoice>(initialTheme);
  const [shortcutError, setShortcutError] = useState("");
  const [saving, setSaving] = useState(false);

  useEffect(() => setSettings(initialSettings), [initialSettings]);
  const loadDevices = async () => {
    setDeviceError("");
    try {
      setDevices(await adapter.listInputDevices());
    } catch (error) {
      setDeviceError(`Nie udało się wczytać mikrofonów: ${normalizeError(error)}`);
    }
  };
  useEffect(() => {
    let active = true;
    void adapter.listInputDevices()
      .then((loaded) => { if (active) setDevices(loaded); })
      .catch((error) => {
        if (active) setDeviceError(`Nie udało się wczytać mikrofonów: ${normalizeError(error)}`);
      });
    void adapter.getModelStatus()
      .then((status) => { if (active) setModelStatus(status); })
      .catch((error) => {
        if (active) setModelStatus({
          state: "error",
          model: "nvidia/parakeet-tdt-0.6b-v3",
          revision: "",
          device: null,
          message: normalizeError(error),
        });
      });
    return () => { active = false; };
  }, [adapter]);
  useEffect(() => {
    applyTheme(theme);
    if (theme !== "system" || !window.matchMedia) return;
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const update = () => applyTheme("system");
    media.addEventListener?.("change", update);
    return () => media.removeEventListener?.("change", update);
  }, [theme]);

  const save = async (next: AppSettings) => {
    const previous = settings;
    setSettings(next);
    setSaving(true);
    try {
      const result = await adapter.updateSettings(next);
      const persisted = await adapter.getSettings().catch(() => result.settings);
      setSettings(persisted);
      onSettingsChange?.(persisted);
      if (result.warning) onToast(result.warning, "info");
    } catch (error) {
      setSettings(previous);
      onToast(`Nie zapisano zmiany: ${normalizeError(error)}`, "error");
    } finally {
      setSaving(false);
    }
  };

  const changeShortcut = (shortcut: string) => {
    setSettings((current) => ({ ...current, shortcut }));
    setShortcutError(shortcutPattern.test(shortcut) ? "" : "Użyj modyfikatora i klawisza, np. Ctrl+Space.");
  };

  return (
    <section className="page settings-page">
      <header className="page-header"><div><p className="eyebrow">Dopasuj Mów</p><h1>Ustawienia</h1><p>Sprzęt, skrót, zachowanie i wygląd.</p></div></header>
      <div className="settings-layout">
        <div className="settings-column">
          <section className="settings-group">
            <div className="group-heading"><Mic size={18} /><div><h2>Nagrywanie</h2><p>Źródło dźwięku i skrót globalny</p></div></div>
            <label className="setting-row"><span><strong>Mikrofon</strong><small>{deviceError || "Urządzenie używane do dyktowania"}</small>{deviceError && <button type="button" className="inline-retry" aria-label="Spróbuj ponownie" onClick={() => void loadDevices()}>Spróbuj ponownie</button>}</span><select disabled={saving || Boolean(deviceError)} value={settings.inputDevice ?? ""} onChange={(event) => void save({ ...settings, inputDevice: event.target.value || null })}><option value="">Domyślny systemowy</option>{devices.map((device) => <option value={device.id} key={device.id}>{device.name}</option>)}</select></label>
            <label className="setting-row"><span><strong>Skrót klawiszowy</strong><small>{shortcutError || "Działa w każdej aplikacji"}</small></span><input disabled={saving} className={shortcutError ? "invalid" : ""} value={settings.shortcut} onChange={(event) => changeShortcut(event.target.value)} onBlur={() => !shortcutError && void save(settings)} aria-invalid={Boolean(shortcutError)} /></label>
          </section>
          <section className="settings-group">
            <div className="group-heading"><h2>Zachowanie</h2></div>
            <label className="setting-row"><span><strong>Wklejaj automatycznie</strong><small>Wstaw tekst do aktywnego okna</small></span><input disabled={saving} type="checkbox" checked={settings.autoPaste} aria-label="Wklejaj automatycznie" onChange={(event) => void save({ ...settings, autoPaste: event.target.checked })} /></label>
            <label className="setting-row"><span><strong>Uruchamiaj z systemem</strong><small>Mów będzie gotowe w zasobniku</small></span><input disabled={saving} type="checkbox" checked={settings.launchOnLogin} onChange={(event) => void save({ ...settings, launchOnLogin: event.target.checked })} /></label>
            <label className="setting-row"><span><strong>Przechowuj nagrania</strong><small>Automatyczne porządkowanie audio</small></span><select disabled={saving} value={settings.retentionDays ?? "forever"} onChange={(event) => void save({ ...settings, retentionDays: event.target.value === "forever" ? null : Number(event.target.value) as 1 | 7 | 30 })}><option value="1">1 dzień</option><option value="7">7 dni</option><option value="30">30 dni</option><option value="forever">Bezterminowo</option></select></label>
          </section>
        </div>
        <div className="settings-column">
          <section className="settings-group">
            <div className="group-heading"><h2>Wygląd</h2><p>Motyw interfejsu</p></div>
            <div className="theme-segment" role="radiogroup" aria-label="Motyw">
              {([["system", "System"], ["light", "Jasny"], ["dark", "Ciemny"]] as const).map(([value, label]) => <label key={value}><input type="radio" name="theme" checked={theme === value} onChange={() => setTheme(value)} /><span>{label}</span></label>)}
            </div>
          </section>
          <section className="model-card" aria-busy={!modelStatus}>
            <div className="model-card__top"><div><span className="model-kicker">Model lokalny</span><h2>NVIDIA Parakeet 0.6B v3</h2></div><span className={`ready-badge ready-badge--${modelStatus?.state ?? "checking"}`}>{modelStatus?.state === "ready" && <Check size={13} />}{!modelStatus ? "Sprawdzam…" : modelStatus.state === "ready" ? "Gotowy" : modelStatus.state === "not_installed" ? "Do pobrania" : "Błąd"}</span></div>
            <p>{modelStatus?.state !== "ready" && modelStatus?.message ? modelStatus.message : "Szybka transkrypcja po polsku bez wysyłania dźwięku do chmury."}</p>
            <div className="model-meta"><span>Urządzenie <strong>{modelStatus?.device ?? "ustalane przy starcie"}</strong></span><span>Dane <strong>tylko lokalnie</strong></span></div>
          </section>
          <aside className="privacy-note"><strong>Twoje słowa zostają u Ciebie</strong><p>Audio, transkrypcje i słownik są przechowywane wyłącznie na tym komputerze.</p></aside>
        </div>
      </div>
    </section>
  );
}
