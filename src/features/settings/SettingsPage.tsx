import { useEffect, useRef, useState } from "react";

import { Check, Trash2 } from "../../components/Icons";
import "./SettingsPage.css";
import { BrandLogo } from "../../components/BrandLogo";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { ShortcutCapture } from "./ShortcutCapture";
import { TextSettings } from "./TextSettings";
import { Select } from "../../components/Select";
import type { ToastKind } from "../../components/Toast";
import type { AppAdapter } from "../../lib/tauri";
import type { AppSettings, DownloadStatus, InputDeviceInfo, MediaDuringDictation, ModelDescriptor, ModelDownloadProgress, ModelStatus, PasteMode } from "../../lib/types";
import { realtimeEnabled, realtimeEngineKey } from "../../lib/types";
import { microphoneChoices, microphonePreferences } from "../../lib/microphones";
import { normalizeError } from "../../lib/errors";
import { formatBytes as formatSize } from "../../lib/bytes";
import { useI18n } from "../../lib/i18n";

function ProviderMark({ provider }: { provider: string }) {
  return <span className={`provider-mark provider-mark--${provider.toLowerCase()}`} aria-label={provider} title={provider}>
    <BrandLogo provider={provider} />
  </span>;
}

/**
 * Stands in for "no device chosen" in the microphone dropdown.
 *
 * The empty string cannot be used: the dropdown reserves it to mean "no value
 * selected", so an option carrying it renders blank. CPAL endpoint ids use
 * the host namespace; this app namespace is reserved for the default choice.
 */
const SYSTEM_DEFAULT_DEVICE = "loquara:system-default";
const UNRESOLVED_DEVICE = "loquara:unresolved";

export function SettingsPage({
  adapter,
  initialSettings,
  download: activeDownload,
  onSettingsChange,
  onModelsChanged,
  onToast,
}: {
  adapter: AppAdapter;
  initialSettings: AppSettings;
  /** The transfer the app has in flight, whichever screen started it. */
  download?: DownloadStatus | null;
  onSettingsChange?: (settings: AppSettings) => void;
  /** Lets the shell re-check readiness after a model is added or removed. */
  onModelsChanged?: () => void;
  onToast: (message: string, kind: ToastKind) => void;
}) {
  const { t } = useI18n();
  const [settings, setSettings] = useState(initialSettings);
  const [tab, setTab] = useState<"dictation" | "text" | "models" | "app">("dictation");
  const [devices, setDevices] = useState<InputDeviceInfo[]>([]);
  const [deviceError, setDeviceError] = useState("");
  const [devicesLoading, setDevicesLoading] = useState(true);
  const [deviceRetry, setDeviceRetry] = useState(0);
  const [modelStatus, setModelStatus] = useState<ModelStatus>();
  const [statusRetry, setStatusRetry] = useState(0);
  const [models, setModels] = useState<ModelDescriptor[]>([]);
  const [modelsLoading, setModelsLoading] = useState(true);
  const [modelsError, setModelsError] = useState("");
  const [modelsRetry, setModelsRetry] = useState(0);
  const [systemMemory, setSystemMemory] = useState<{ vramGb: number; ramGb: number; cpuCores: number }>({ vramGb: 0, ramGb: 0, cpuCores: 0 });
  // Whether something is downloading is the app's business, not this
  // screen's: this screen can be left and re-entered mid-transfer.
  const downloading = activeDownload?.model ?? "";
  const [deleting, setDeleting] = useState("");
  const [pendingDelete, setPendingDelete] = useState<ModelDescriptor>();
  const [downloadProgress, setDownloadProgress] = useState<ModelDownloadProgress>();

  const formatBytes = (bytes: number | null | undefined) =>
    formatSize(bytes, t("settings.models.notDownloadedBytes"));

  useEffect(() => setSettings(initialSettings), [initialSettings]);
  useEffect(() => {
    let active = true;
    setDevicesLoading(true);
    setDeviceError("");
    void adapter.listInputDevices()
      .then((loaded) => { if (active) setDevices(loaded); })
      .catch((error) => {
        if (active) setDeviceError(t("settings.mic.error", { error: normalizeError(error) }));
      }).finally(() => { if (active) setDevicesLoading(false); });
    return () => { active = false; };
  }, [adapter, deviceRetry, t]);

  useEffect(() => {
    let active = true;
    let dispose: (() => void) | undefined;
    void adapter.onInputDevicesChanged(() => { if (active) setDeviceRetry((value) => value + 1); })
      .then((unlisten) => { if (active) dispose = unlisten; else unlisten(); })
      .catch((error) => { if (active) setDeviceError(normalizeError(error)); });
    return () => { active = false; dispose?.(); };
  }, [adapter]);

  useEffect(() => {
    let active = true;
    setModelsLoading(true);
    setModelsError("");
    void adapter.listModels()
      .then((loaded) => { if (active) setModels(loaded); })
      .catch((error) => { if (active) setModelsError(normalizeError(error)); })
      .finally(() => { if (active) setModelsLoading(false); });
    return () => { active = false; };
  }, [adapter, modelsRetry]);

  useEffect(() => {
    let active = true;
    setModelStatus(undefined);
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
  }, [adapter, settings.model, statusRetry]);

  useEffect(() => {
      const ramGb = (navigator as unknown as { deviceMemory?: number }).deviceMemory ?? 0;
      const vramGb = 0;
      setSystemMemory({ vramGb, ramGb, cpuCores: navigator.hardwareConcurrency ?? 0 });
  }, []);

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;
    void adapter.onModelProgress((progress) => {
      if (active) setDownloadProgress(progress);
    }).then((dispose) => {
      if (active) unlisten = dispose;
      else dispose();
    });
    return () => {
      active = false;
      unlisten?.();
    };
  }, [adapter]);

  // Mirrors `settings` so a save always composes against the newest values,
  // even when two controls are changed faster than React re-renders.
  const latest = useRef(initialSettings);
  useEffect(() => { latest.current = settings; }, [settings]);

  /**
   * Applies one change and persists the whole settings object.
   *
   * Takes a patch rather than a fully built object so callers cannot base a
   * save on values that have since moved on. Nothing is disabled while this
   * runs: the change is applied optimistically and rolled back if the backend
   * rejects it, so blocking the rest of the page buys nothing and makes every
   * other control flash as it greys out and back.
   */
  const save = async (patch: Partial<AppSettings>) => {
    const previous = latest.current;
    const next = { ...previous, ...patch, overlaySize: "mini" as const };
    latest.current = next;
    setSettings(next);
    try {
      const result = await adapter.updateSettings(next);
      const persisted = await adapter.getSettings().catch(() => result.settings);
      latest.current = persisted;
      setSettings(persisted);
      onSettingsChange?.(persisted);
      if (result.warning) onToast(result.warning, "info");
    } catch (error) {
      latest.current = previous;
      setSettings(previous);
      onToast(t("settings.saveError", { error: normalizeError(error) }), "error");
    }
  };

  const microphones = microphonePreferences(settings, devices);
  const microphoneOptions = microphoneChoices(microphones, devices);
  const saveMicrophones = (patch: Partial<typeof microphones>) => {
    const next = { ...microphonePreferences(latest.current, devices), ...patch };
    void save({ microphones: next, inputDevice: next.selectedId });
  };
  const deviceLabel = (device: InputDeviceInfo) => {
    // Labels may change; the opaque endpoint ID is only the persisted key.
    const sameName = microphoneOptions.filter((item) => item.name === device.name);
    const suffix = sameName.length > 1 ? ` (${sameName.findIndex((item) => item.id === device.id) + 1})` : "";
    return `${device.name}${suffix}${device.available ? "" : ` — ${t("settings.mic.unavailable")}`}`;
  };
  const moveFavorite = (id: string, direction: -1 | 1) => {
    const next = [...microphones.favorites];
    const index = next.findIndex((favorite) => favorite.id === id);
    const target = index + direction;
    if (index < 0 || target < 0 || target >= next.length) return;
    [next[index], next[target]] = [next[target], next[index]];
    saveMicrophones({ favorites: next });
  };


  const download = async (key: string) => {
    setDownloadProgress({ model: key, phase: "preparing", downloadedBytes: 0, totalBytes: null });
    try {
      await adapter.downloadModel(key);
      const loaded = await adapter.listModels();
      setModels(loaded);
      const status = await adapter.getModelStatus();
      setModelStatus(status);
      onModelsChanged?.();
      onToast(t("settings.models.downloadedToast", { model: models.find((model) => model.key === key)?.display ?? key }), "success");
    } catch (error) {
      onToast(t("settings.models.downloadError", { error: normalizeError(error) }), "error");
    } finally {
      setDownloadProgress(undefined);
    }
  };

  const remove = async (model: ModelDescriptor) => {
    setDeleting(model.key);
    try {
      await adapter.deleteModel(model.key);
      setModels(await adapter.listModels());
      onModelsChanged?.();
      onToast(t("settings.models.removedToast", { model: model.display }), "success");
    } catch (error) {
      onToast(t("settings.models.removeError", { error: normalizeError(error) }), "error");
    } finally {
      setDeleting("");
    }
  };

  const realtimeSupported = settings.model === "parakeet";
  const selectedModel = models.find((model) => model.key === settings.model);
  const selectedState = modelStatus?.state ?? selectedModel?.status;
  const selectedStatusLabel = !modelStatus
    ? t("settings.models.checking")
    : selectedState === "ready"
      ? t("common.ready")
      : selectedState === "error"
        ? t("common.error")
        : t("settings.models.notInstalled");

  return (
    <section className="page settings-page">
      <header className="page-header settings-header">
        <h1>{t("settings.title")}</h1>

      <div className="settings-tabs" role="tablist" aria-label={t("settings.tabs")}>
        {(["dictation", "text", "models", "app"] as const).map((item) => <button type="button" key={item} id={`settings-tab-${item}`}
          role="tab" aria-selected={tab === item} aria-controls={`settings-panel-${item}`} tabIndex={tab === item ? 0 : -1}
          onClick={() => setTab(item)} onKeyDown={(event) => {
            const items = ["dictation", "text", "models", "app"] as const;
            const index = items.indexOf(item);
            const next = event.key === "ArrowRight" ? items[(index + 1) % items.length]
              : event.key === "ArrowLeft" ? items[(index + items.length - 1) % items.length]
              : event.key === "Home" ? items[0] : event.key === "End" ? items[items.length - 1] : undefined;
            if (next) { event.preventDefault(); setTab(next); document.getElementById(`settings-tab-${next}`)?.focus(); }
          }}>{t(`settings.tab.${item}`)}</button>)}
      </div>
      </header>
      <div className="settings-column" role="tabpanel" id={`settings-panel-${tab}`} aria-labelledby={`settings-tab-${tab}`}>

        {tab === "models" ? <section className="settings-group settings-models">
          <p className="settings-source-privacy" id="settings-source-privacy">{t("settings.privacy.body")}</p>
          {modelsLoading ? <p className="settings-feedback" role="status">{t("settings.models.checking")}</p>
            : modelsError ? <div className="settings-feedback settings-feedback--error" role="alert"><span>{t("settings.models.loadError", { error: modelsError })}</span><button className="text-button" type="button" onClick={() => setModelsRetry((value) => value + 1)}>{t("settings.retry")}</button></div>
            : !models.length ? <div className="settings-feedback" role="status"><span>{t("settings.models.empty")}</span><button className="text-button" type="button" onClick={() => setModelsRetry((value) => value + 1)}>{t("settings.retry")}</button></div> : null}
          <div className="model-options" role="radiogroup" aria-label={t("settings.models.heading")} aria-busy={modelsLoading}>
            {models.map((model) => {
              const lacksRam = systemMemory.ramGb > 0 && model.minRamGb > systemMemory.ramGb;
              const disabled = lacksRam;
              const selected = settings.model === model.key;
              const isDownloading = downloading === model.key;
              // Live events while this screen is open, the app's own record
              // when it was opened mid-transfer — either way the bar is the
              // transfer that is actually running.
              const progress = !isDownloading
                ? undefined
                : downloadProgress?.model === model.key
                  ? downloadProgress
                  : activeDownload && activeDownload.model === model.key
                    ? {
                        model: activeDownload.model,
                        phase: "downloading",
                        downloadedBytes: activeDownload.downloadedBytes,
                        totalBytes: activeDownload.totalBytes,
                      }
                    : undefined;
              const progressPercent = progress?.totalBytes && progress.totalBytes > 0
                ? Math.min(100, Math.round((progress.downloadedBytes / progress.totalBytes) * 100))
                : null;
              const isDeleting = deleting === model.key;
              const installed = model.status === "ready";
              return (
                <label key={model.key} className={`model-option ${selected ? "model-option--selected" : ""} ${disabled ? "model-option--disabled" : ""}`}>
                  <input
                    className="sr-only"
                    type="radio"
                    name="model"
                    value={model.key}
                    checked={selected}
                    disabled={disabled || modelsLoading || Boolean(modelsError)}
                    onChange={() => void save({ model: model.key })}
                  />
                  <span className="model-option__identity">
                    <span className="model-radio" aria-hidden="true" />
                    <ProviderMark provider={model.provider} />
                    <span className="model-option__name">
                      <strong title={model.display}>{model.display}</strong>
                      <small>
                        {model.provider} &middot; {model.languages} &middot; {installed ? formatBytes(model.installedSizeBytes) : `~${formatBytes(model.estimatedSizeBytes)}`}
                        {lacksRam ? ` · ${t("settings.models.needsRam", { gb: model.minRamGb })}` : ""}
                      </small>
                    </span>
                  </span>
                  <span className="model-option__meta">
                    <span className={`model-option__status model-option__status--${model.status}`}>
                      <i aria-hidden="true" />
                      {model.status === "ready" ? t("settings.models.status.downloaded") : model.status === "error" ? t("common.error") : t("settings.models.status.notDownloaded")}
                    </span>
                    {!installed && !disabled ? (
                      <button
                        type="button"
                        className={`model-download ${isDownloading ? "model-download--active" : ""}`}
                        disabled={Boolean(downloading) || Boolean(deleting)}
                        onClick={(event) => { event.preventDefault(); void download(model.key); }}
                        aria-label={t("settings.models.downloadAria", { model: model.display })}
                      >
                        {isDownloading
                          ? progress?.phase === "validating"
                            ? t("settings.models.checking")
                            : progressPercent === null ? t("settings.models.preparingShort") : `${progressPercent}%`
                          : t("settings.models.download")}
                      </button>
                    ) : null}
                    {installed && !selected ? (
                      <button
                        type="button"
                        className="model-remove"
                        disabled={Boolean(downloading) || Boolean(deleting)}
                        onClick={(event) => { event.preventDefault(); setPendingDelete(model); }}
                        aria-label={t("settings.models.removeAria", { model: model.display })}
                        title={t("settings.models.removeTitle")}
                      >
                        {isDeleting ? "…" : <Trash2 size={14} />}
                      </button>
                    ) : null}
                  </span>
                  {progress ? (
                    <span className="model-download-progress" aria-label={progressPercent === null ? t("settings.models.progress.preparing") : t("settings.models.progress.downloaded", { percent: progressPercent })}>
                      <span style={{ width: `${progressPercent ?? 4}%` }} />
                    </span>
                  ) : null}
                </label>
              );
            })}
          </div>
          <p className="model-footnote">
            <span>{t("settings.models.active")} <strong>{selectedModel?.display ?? t("settings.models.selecting")}</strong></span>
            <span>{t("settings.models.thisComputer")} <strong>{systemMemory.cpuCores ? t("settings.models.cores", { cores: systemMemory.cpuCores }) : ""}{systemMemory.ramGb ? `${systemMemory.ramGb} GB RAM` : t("settings.models.ramUnknown")}</strong></span>
            {selectedModel?.status === "ready" ? (
              <button type="button" onClick={() => void adapter.openModelFolder(selectedModel.key)}>
                {t("settings.models.openFolder")}
              </button>
            ) : null}
          </p>
          {(modelStatus?.message ?? selectedModel?.message) && selectedState !== "ready"
            ? <div className="settings-feedback settings-feedback--error" role="alert"><span>{modelStatus?.message ?? selectedModel?.message}</span><button className="text-button" type="button" onClick={() => setStatusRetry((value) => value + 1)}>{t("settings.retry")}</button></div>
            : null}
          <section className="settings-group settings-group--composed settings-model-memory">
            <div className="group-heading"><h2>{t("settings.modelKeepAlive.title")}</h2><p>{t("settings.modelKeepAlive.help")}</p></div>
            <div className="settings-controls">
          <label className="setting-row">
            <span><strong>{t("settings.modelKeepAlive.label")}</strong><small>{t("settings.modelKeepAlive.description")}</small></span>
            <Select
              label={t("settings.modelKeepAlive.label")}
              value={String(settings.modelKeepAliveSecs)}
              onChange={(next) => void save({ modelKeepAliveSecs: Number(next) })}
              options={[
                { value: "0", label: t("settings.modelKeepAlive.always") },
                { value: "60", label: t("settings.modelKeepAlive.min1") },
                { value: "180", label: t("settings.modelKeepAlive.min3") },
                { value: "300", label: t("settings.modelKeepAlive.min5") },
                { value: "600", label: t("settings.modelKeepAlive.min10") },
              ]}
            />
          </label>
            </div>
          </section>
        </section> : null}
        {tab === "dictation" ? <section className="settings-shortcut">
          <div className="settings-shortcut__copy"><h2>{t("settings.shortcut.title")}</h2><p>{t(settings.activationMode === "push_to_talk" ? "settings.shortcut.holdHelp" : "settings.shortcut.help")}</p></div>
          <div className="settings-shortcut__controls">
            <div className="settings-shortcut__mode">
              <label htmlFor="shortcut-activation-mode">{t("settings.shortcut.activationMode")}</label>
              <Select
                id="shortcut-activation-mode"
                label={t("settings.shortcut.activationMode")}
                value={settings.activationMode ?? "toggle"}
                options={[
                  { value: "toggle", label: t("settings.shortcut.toggle") },
                  { value: "push_to_talk", label: t("settings.shortcut.hold") },
                ]}
                onChange={(value) => void save({ activationMode: value as AppSettings["activationMode"] })}
              />
            </div>
            <ShortcutCapture value={settings.shortcut} onCapture={(combo) => void save({ shortcut: combo })} onActiveChange={(active) => { void adapter.setShortcutSuspended(active).catch(() => undefined); }} />
          </div>
        </section> : null}
        {tab === "dictation" ? <section className="settings-group settings-group--composed">
          <div className="group-heading"><h2>{t("settings.recording.title")}</h2><p>{t("settings.recording.help")}</p></div>
          <div className="settings-controls">
          <label className="setting-row">
            <span><strong>{t("settings.mic.label")}</strong></span>
            <Select
              label={t("settings.mic.label")}
              disabled={devicesLoading || Boolean(deviceError) || microphones.selection === "favorites"}
              value={microphones.unresolvedLegacyName ? UNRESOLVED_DEVICE : microphones.selectedId ?? SYSTEM_DEFAULT_DEVICE}
              onChange={(next) => saveMicrophones({ selectedId: next === SYSTEM_DEFAULT_DEVICE ? null : next, unresolvedLegacyName: null,
                hiddenIds: microphones.hiddenIds.filter((id) => id !== next) })}
              options={[
                { value: SYSTEM_DEFAULT_DEVICE, label: t("settings.mic.default") },
                ...(microphones.unresolvedLegacyName ? [{ value: UNRESOLVED_DEVICE, label: `${microphones.unresolvedLegacyName} — ${t("settings.mic.choose")}` }] : []),
                ...microphoneOptions.filter((device) => !microphones.hiddenIds.includes(device.id) || device.id === microphones.selectedId).map((device) => ({ value: device.id, label: deviceLabel(device) })),
              ]}
            />
          </label>
          {devicesLoading ? <p className="settings-feedback" role="status">{t("settings.models.checking")}</p>
            : deviceError || !devices.length ? <div className={`settings-feedback ${deviceError ? "settings-feedback--error" : ""}`} role={deviceError ? "alert" : "status"}>
              <span>{deviceError || t("settings.mic.empty")}</span>
              <button className="text-button" type="button" onClick={() => setDeviceRetry((value) => value + 1)}>{t("settings.retry")}</button>
            </div> : null}
          {microphones.unresolvedLegacyName ? <p className="settings-feedback settings-feedback--error" role="alert">{t("settings.mic.legacy")}</p> : null}
          {microphones.selection === "manual" && microphoneOptions.some((device) => microphones.hiddenIds.includes(device.id) && (microphones.selectedId === device.id || microphones.selectedId === null && device.isDefault)) ? <p className="settings-feedback" role="status">{t("settings.mic.selectedHidden")}</p> : null}
          <details className="settings-microphones">
            <summary>{t("settings.mic.manage")}</summary>
            <label className="setting-row">
              <span><strong>{t("settings.mic.automatic")}</strong><small>{t("settings.mic.automaticHelp")}</small></span>
              <input type="checkbox" aria-label={t("settings.mic.automatic")} checked={microphones.selection === "favorites"}
                onChange={(event) => saveMicrophones({ selection: event.target.checked ? "favorites" : "manual" })} />
            </label>
            {microphones.selection === "favorites" ? <label className="setting-row">
              <span><strong>{t("settings.mic.fallback")}</strong></span>
              <input type="checkbox" aria-label={t("settings.mic.fallback")} checked={microphones.allowSystemDefaultFallback}
                onChange={(event) => saveMicrophones({ allowSystemDefaultFallback: event.target.checked })} />
            </label> : null}
            {microphones.selection === "favorites" && !microphones.favorites.some((favorite) => microphoneOptions.some((device) => device.id === favorite.id && device.available && !microphones.hiddenIds.includes(device.id))) && !microphones.allowSystemDefaultFallback
              ? <p className="settings-feedback" role="status">{t("settings.mic.noFavorite")}</p> : null}
            <ol className="settings-microphones__favorites" aria-label={t("settings.mic.favorites")}>
              {microphones.favorites.map((favorite, index) => {
                const device = microphoneOptions.find((item) => item.id === favorite.id)!;
                return <li key={favorite.id}>
                  <span>{deviceLabel(device)}{microphones.hiddenIds.includes(favorite.id) ? ` — ${t("settings.mic.hidden")}` : ""}</span>
                  <div className="settings-microphones__actions">
                    <button type="button" className="text-button" aria-label={t("settings.mic.up", { name: device.name })} disabled={index === 0} onClick={() => moveFavorite(favorite.id, -1)}>{t("settings.mic.upShort")}</button>
                    <button type="button" className="text-button" aria-label={t("settings.mic.down", { name: device.name })} disabled={index === microphones.favorites.length - 1} onClick={() => moveFavorite(favorite.id, 1)}>{t("settings.mic.downShort")}</button>
                    <button type="button" className="text-button" aria-label={t("settings.mic.remove", { name: device.name })} onClick={() => saveMicrophones({ favorites: microphones.favorites.filter((item) => item.id !== favorite.id) })}>{t("settings.mic.removeShort")}</button>
                  </div>
                </li>;
              })}
            </ol>
            <ul className="settings-microphones__devices">
              {microphoneOptions.map((device) => {
                const hidden = microphones.hiddenIds.includes(device.id);
                return <li key={device.id}>
                  <span>{deviceLabel(device)}</span>
                  <div className="settings-microphones__actions">
                    {!microphones.favorites.some((favorite) => favorite.id === device.id) ? <button type="button" className="text-button" aria-label={t("settings.mic.add", { name: device.name })} onClick={() => saveMicrophones({ favorites: [...microphones.favorites, { id: device.id, name: device.name }] })}>{t("settings.mic.addShort")}</button> : null}
                    <button type="button" className="text-button" aria-label={t(hidden ? "settings.mic.restore" : "settings.mic.hide", { name: device.name })}
                      disabled={!hidden && microphones.selection === "manual" && (device.id === microphones.selectedId || microphones.selectedId === null && device.isDefault)}
                      onClick={() => saveMicrophones({ hiddenIds: hidden ? microphones.hiddenIds.filter((id) => id !== device.id) : [...microphones.hiddenIds, device.id] })}>{t(hidden ? "settings.mic.restoreShort" : "settings.mic.hideShort")}</button>
                  </div>
                </li>;
              })}
            </ul>
          </details>
          <label className="setting-row setting-row--realtime">
            <span><strong>{t("settings.realtime.label")}</strong><small>{t(realtimeSupported ? "settings.realtime.description" : "settings.realtime.unsupported")}</small></span>
            <input type="checkbox" aria-label={t("settings.realtime.label")} checked={realtimeEnabled(settings, realtimeSupported)} disabled={!realtimeSupported}
              onChange={(event) => void save({ realtimeByEngine: { ...latest.current.realtimeByEngine, [realtimeEngineKey(latest.current)]: event.target.checked } })} />
          </label>
          <label className="setting-row">
            <span><strong>{t("settings.media.label")}</strong><small>{t("settings.media.description")}</small></span>
            <Select
              label={t("settings.media.label")}
              value={settings.mediaDuringDictation ?? "muffle"}
              onChange={(next) => void save({ mediaDuringDictation: next as MediaDuringDictation })}
              options={[
                { value: "muffle", label: t("settings.media.muffle") },
                { value: "duck", label: t("settings.media.duck") },
                { value: "pause", label: t("settings.media.pause") },
                { value: "off", label: t("settings.media.off") },
              ]}
            />
          </label>
          <div className="settings-engine">
            <span className={`group-status group-status--${selectedState ?? "checking"}`}>
              {selectedState === "ready" ? <Check size={13} /> : null}
              <span>{selectedModel?.display ?? settings.model}</span>
              <span className="settings-engine__status">{selectedStatusLabel}</span>
            </span>
            <button type="button" className="text-button" onClick={() => setTab("models")}>{t("settings.manageModels")}</button>
          </div>
          {modelStatus?.message && selectedState !== "ready"
            ? <div className="settings-feedback settings-feedback--error" role="alert"><span>{modelStatus.message}</span><button className="text-button" type="button" onClick={() => setStatusRetry((value) => value + 1)}>{t("settings.retry")}</button></div> : null}

          </div>
        </section> : null}
        {tab === "dictation" ? <section className="settings-group settings-group--composed">
          <div className="group-heading"><h2>{t("settings.delivery.title")}</h2><p>{t("settings.delivery.help")}</p></div>
          <div className="settings-controls">
          <label className="setting-row">
            <span><strong>{t("settings.autoPaste.label")}</strong></span>
            <input type="checkbox" checked={settings.autoPaste} aria-label={t("settings.autoPaste.label")} onChange={(event) => void save({ autoPaste: event.target.checked })} />
          </label>
          <label className="setting-row">
            <span><strong>{t("settings.pasteMode.label")}</strong></span>
            <Select
              label={t("settings.pasteMode.label")}
              value={settings.pasteMode ?? "auto"}
              onChange={(next) => void save({ pasteMode: next as PasteMode })}
              options={[
                { value: "auto", label: t("settings.pasteMode.auto") },
                { value: "ctrl_shift_v", label: t("settings.pasteMode.ctrlShiftV") },
                { value: "shift_insert", label: t("settings.pasteMode.shiftInsert") },
                { value: "ctrl_v", label: t("settings.pasteMode.ctrlV") },
              ]}
            />
          </label>
          <label className="setting-row">
            <span><strong>{t("settings.showOverlay.label")}</strong></span>
            <input type="checkbox" checked={settings.showOverlay} aria-label={t("settings.showOverlay.label")} onChange={(event) => void save({ showOverlay: event.target.checked })} />
          </label>
          </div>
        </section> : null}
        {tab === "text" ? <TextSettings settings={settings} save={save} /> : null}
        {tab === "app" ? <section className="settings-group settings-group--composed">
          <div className="group-heading"><h2>{t("settings.app.startup.title")}</h2><p>{t("settings.app.startup.help")}</p></div>
          <div className="settings-controls">
          <label className="setting-row">
            <span><strong>{t("settings.launchOnLogin.label")}</strong><small>{t("settings.launchOnLogin.description")}</small></span>
            <input type="checkbox" checked={settings.launchOnLogin} aria-label={t("settings.launchOnLogin.label")} onChange={(event) => void save({ launchOnLogin: event.target.checked })} />
          </label>
          <label className="setting-row">
            <span><strong>{t("settings.startMinimized.label")}</strong><small>{t("settings.startMinimized.description")}</small></span>
            <input type="checkbox" checked={settings.startMinimized} aria-label={t("settings.startMinimized.label")} onChange={(event) => void save({ startMinimized: event.target.checked })} />
          </label>
          </div>
        </section> : null}
        {tab === "app" ? <section className="settings-group settings-group--composed">
          <div className="group-heading"><h2>{t("settings.app.history.title")}</h2><p>{t("settings.app.history.help")}</p></div>
          <div className="settings-controls">
          <label className="setting-row">
            <span><strong>{t("settings.retention.label")}</strong><small>{t("settings.retention.description")}</small></span>
            <Select
              label={t("settings.retention.label")}
              value={settings.retentionDays === null ? "forever" : String(settings.retentionDays)}
              onChange={(next) => void save({ retentionDays: next === "forever" ? null : Number(next) as 1 | 7 | 30 })}
              options={[
                { value: "1", label: t("settings.retention.1") },
                { value: "7", label: t("settings.retention.7") },
                { value: "30", label: t("settings.retention.30") },
                { value: "forever", label: t("settings.retention.forever") },
              ]}
            />
          </label>
          </div>
        </section> : null}
        {tab === "app" ? <section className="settings-group settings-group--composed">
          <div className="group-heading"><h2>{t("settings.general.title")}</h2><p>{t("settings.general.subtitle")}</p></div>
          <div className="settings-controls">
          <div className="setting-block">
            <strong>{t("settings.language.label")}</strong>
            <div className="segment" role="radiogroup" aria-label={t("settings.language.label")}>
              {([["system", "settings.language.system"], ["pl", "settings.language.pl"], ["en", "settings.language.en"]] as const).map(([value, label]) => (
                <label key={value}><input type="radio" name="language" checked={settings.language === value} onChange={() => void save({ language: value })} /><span>{t(label)}</span></label>
              ))}
            </div>
          </div>
          </div>
        </section> : null}
        {tab !== "dictation" && tab !== "models" ? <p className="privacy-line">
          <strong>{t("settings.privacy.title")}</strong>
          <span>{t("settings.privacy.body")}</span>
        </p> : null}
      </div>

      <ConfirmDialog
         open={Boolean(pendingDelete)}
         title={t("settings.models.removeConfirm", { model: pendingDelete?.display ?? "" })}
         message={t("settings.models.removeConfirmMessage", { model: pendingDelete?.display ?? "", size: pendingDelete ? formatBytes(pendingDelete.installedSizeBytes ?? pendingDelete.estimatedSizeBytes) : "" })}
         confirmLabel={t("common.delete")}
         cancelLabel={t("common.cancel")}
         danger
         busy={Boolean(deleting)}
         onCancel={() => setPendingDelete(undefined)}
         onConfirm={() => {
           if (pendingDelete) void remove(pendingDelete);
           setPendingDelete(undefined);
         }}
       />
    </section>
  );
}
