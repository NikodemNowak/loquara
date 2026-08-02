import { useEffect, useState } from "react";

import { Check, Cpu, Languages, Mic, Palette, SlidersHorizontal, Trash2 } from "../../components/Icons";
import { BrandLogo } from "../../components/BrandLogo";
import { ShortcutCapture } from "./ShortcutCapture";
import { applyTheme } from "../../app/theme";
import type { ToastKind } from "../../components/Toast";
import type { AppAdapter } from "../../lib/tauri";
import type { AppSettings, InputDeviceInfo, ModelDescriptor, ModelDownloadProgress, ModelStatus } from "../../lib/types";
import { normalizeError } from "../../lib/errors";
import { useI18n } from "../../lib/i18n";

function ProviderMark({ provider }: { provider: string }) {
  return <span className={`provider-mark provider-mark--${provider.toLowerCase()}`} aria-label={provider} title={provider}>
    <BrandLogo provider={provider} />
  </span>;
}

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
  const { t } = useI18n();
  const [settings, setSettings] = useState(initialSettings);
  const [devices, setDevices] = useState<InputDeviceInfo[]>([]);
  const [deviceError, setDeviceError] = useState("");
  const [modelStatus, setModelStatus] = useState<ModelStatus>();
  const [models, setModels] = useState<ModelDescriptor[]>([]);
  const [systemMemory, setSystemMemory] = useState<{ vramGb: number; ramGb: number; cpuCores: number }>({ vramGb: 0, ramGb: 0, cpuCores: 0 });
  const [saving, setSaving] = useState(false);
  const [downloading, setDownloading] = useState("");
  const [deleting, setDeleting] = useState("");
  const [downloadProgress, setDownloadProgress] = useState<ModelDownloadProgress>();

  const formatBytes = (bytes: number | null | undefined) => {
    if (!bytes) return t("settings.models.notDownloadedBytes");
    const units = ["B", "KB", "MB", "GB", "TB"];
    let value = bytes;
    let unit = 0;
    while (value >= 1000 && unit < units.length - 1) {
      value /= 1000;
      unit += 1;
    }
    return `${value >= 10 || unit === 0 ? value.toFixed(0) : value.toFixed(1)} ${units[unit]}`;
  };

  useEffect(() => setSettings(initialSettings), [initialSettings]);
  useEffect(() => {
    let active = true;
    void adapter.listInputDevices()
      .then((loaded) => { if (active) setDevices(loaded); })
      .catch((error) => {
        if (active) setDeviceError(t("settings.mic.error", { error: normalizeError(error) }));
      });
    void adapter.listModels()
      .then((loaded) => { if (active) setModels(loaded); })
      .catch(() => {});
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
    void (async () => {
      const ramGb = (navigator as unknown as { deviceMemory?: number }).deviceMemory ?? 0;
      const vramGb = 0;
      if (active) setSystemMemory({ vramGb, ramGb, cpuCores: navigator.hardwareConcurrency ?? 0 });
    })();
    return () => { active = false; };
  }, [adapter, settings.model, t]);
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
  useEffect(() => {
    applyTheme(initialSettings.theme);
  }, [initialSettings.theme]);
  useEffect(() => {
    applyTheme(settings.theme);
    if (settings.theme !== "system" || !window.matchMedia) return;
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const update = () => applyTheme("system");
    media.addEventListener?.("change", update);
    return () => media.removeEventListener?.("change", update);
  }, [settings.theme]);

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
      onToast(t("settings.saveError", { error: normalizeError(error) }), "error");
    } finally {
      setSaving(false);
    }
  };


  const download = async (key: string) => {
    setDownloading(key);
    setDownloadProgress({ model: key, phase: "preparing", downloadedBytes: 0, totalBytes: null });
    try {
      await adapter.downloadModel(key);
      const loaded = await adapter.listModels();
      setModels(loaded);
      const status = await adapter.getModelStatus();
      setModelStatus(status);
      onToast(t("settings.models.downloadedToast", { model: models.find((model) => model.key === key)?.display ?? key }), "success");
    } catch (error) {
      onToast(t("settings.models.downloadError", { error: normalizeError(error) }), "error");
    } finally {
      setDownloading("");
      setDownloadProgress(undefined);
    }
  };

  const remove = async (model: ModelDescriptor) => {
    if (!window.confirm(t("settings.models.removeConfirm", { model: model.display }))) return;
    setDeleting(model.key);
    try {
      await adapter.deleteModel(model.key);
      setModels(await adapter.listModels());
      onToast(t("settings.models.removedToast", { model: model.display }), "success");
    } catch (error) {
      onToast(t("settings.models.removeError", { error: normalizeError(error) }), "error");
    } finally {
      setDeleting("");
    }
  };

  const selectedModel = models.find((model) => model.key === settings.model);
  const selectedState = modelStatus?.state ?? selectedModel?.status;
  const selectedStatusLabel = !models.length
    ? t("settings.models.checking")
    : selectedState === "ready"
      ? t("common.ready")
      : selectedState === "error"
        ? t("common.error")
        : t("settings.models.notInstalled");

  return (
    <section className="page settings-page">
       <header className="page-header"><div><p className="eyebrow">Loquara</p><h1>{t("settings.title")}</h1><p>{t("settings.subtitle")}</p></div></header>
      <div className="settings-layout">
        <div className="settings-column">
          <section className="settings-group">
            <div className="group-heading"><Mic size={18} /><div><h2>{t("settings.recording.title")}</h2><p>{t("settings.recording.subtitle")}</p></div></div>
            <label className="setting-row"><span><strong>{t("settings.mic.label")}</strong><small>{deviceError || t("settings.mic.description")}</small></span><select disabled={saving || Boolean(deviceError)} value={settings.inputDevice ?? ""} onChange={(event) => void save({ ...settings, inputDevice: event.target.value || null })}><option value="">{t("settings.mic.default")}</option>{devices.map((device) => <option value={device.id} key={device.id}>{device.name}</option>)}</select></label>
            <div className="setting-row"><span><strong>{t("settings.shortcut.label")}</strong><small>{t("settings.shortcut.description")}</small></span><ShortcutCapture value={settings.shortcut} disabled={saving} onCapture={(combo) => void save({ ...settings, shortcut: combo })} onActiveChange={(active) => { void adapter.setShortcutSuspended(active).catch(() => undefined); }} /></div>
          </section>
          <section className="settings-group">
            <div className="group-heading"><SlidersHorizontal size={18} /><div><h2>{t("settings.behavior.title")}</h2></div></div>
            <label className="setting-row"><span><strong>{t("settings.autoPaste.label")}</strong><small>{t("settings.autoPaste.description")}</small></span><input disabled={saving} type="checkbox" checked={settings.autoPaste} aria-label={t("settings.autoPaste.label")} onChange={(event) => void save({ ...settings, autoPaste: event.target.checked })} /></label>
            <label className="setting-row"><span><strong>{t("settings.showOverlay.label")}</strong><small>{t("settings.showOverlay.description")}</small></span><input disabled={saving} type="checkbox" checked={settings.showOverlay} aria-label={t("settings.showOverlay.label")} onChange={(event) => void save({ ...settings, showOverlay: event.target.checked })} /></label>
             <label className="setting-row"><span><strong>{t("settings.launchOnLogin.label")}</strong><small>{t("settings.launchOnLogin.description")}</small></span><input disabled={saving} type="checkbox" checked={settings.launchOnLogin} onChange={(event) => void save({ ...settings, launchOnLogin: event.target.checked })} /></label>
            <label className="setting-row"><span><strong>{t("settings.retention.label")}</strong><small>{t("settings.retention.description")}</small></span><select disabled={saving} value={settings.retentionDays ?? "forever"} onChange={(event) => void save({ ...settings, retentionDays: event.target.value === "forever" ? null : Number(event.target.value) as 1 | 7 | 30 })}><option value="1">{t("settings.retention.1")}</option><option value="7">{t("settings.retention.7")}</option><option value="30">{t("settings.retention.30")}</option><option value="forever">{t("settings.retention.forever")}</option></select></label>
          </section>
        </div>
        <div className="settings-column">
          <section className="settings-group">
            <div className="group-heading"><Palette size={18} /><div><h2>{t("settings.appearance.title")}</h2><p>{t("settings.appearance.subtitle")}</p></div></div>
            <div className="theme-segment" role="radiogroup" aria-label={t("settings.theme.label")}>
              {([["system", "settings.theme.system"], ["light", "settings.theme.light"], ["dark", "settings.theme.dark"]] as const).map(([value, label]) => <label key={value}><input type="radio" name="theme" checked={settings.theme === value} onChange={() => { applyTheme(value); void save({ ...settings, theme: value }); }} /><span>{t(label)}</span></label>)}
            </div>
          </section>
          <section className="settings-group">
            <div className="group-heading"><Languages size={18} /><div><h2>{t("settings.general.title")}</h2><p>{t("settings.general.subtitle")}</p></div></div>
            <div className="theme-segment" role="radiogroup" aria-label={t("settings.language.label")}>
              {([["system", "settings.language.system"], ["pl", "settings.language.pl"], ["en", "settings.language.en"]] as const).map(([value, label]) => <label key={value}><input type="radio" name="language" checked={settings.language === value} onChange={() => void save({ ...settings, language: value })} /><span>{t(label)}</span></label>)}
            </div>
          </section>
        </div>
        <section className="model-card model-library" aria-busy={!models.length}>
             <div className="model-card__top"><div><span className="model-kicker"><Cpu size={11} /> {t("settings.models.kicker")}</span><h2>{t("settings.models.heading")}</h2></div><span className={`ready-badge ready-badge--${selectedState ?? "checking"}`}>{selectedState === "ready" && <Check size={13} />}{selectedStatusLabel}</span></div>
             <p className="model-card__intro">{t("settings.models.intro")}{downloadProgress ? <strong className="model-progress-label"> {downloadProgress.phase === "preparing" ? t("settings.models.preparingFiles") : downloadProgress.phase === "validating" ? t("settings.models.validating") : t("settings.models.downloading")}</strong> : null}</p>
             <div className="model-library__legend"><span>{t("settings.models.legend.model")}</span><span>{t("settings.models.legend.source")}</span><span>{t("settings.models.legend.size")}</span></div>
             <div className="model-options" role="radiogroup" aria-label={t("settings.models.kicker")}>
               {models.map((model) => {
                 const lacksRam = systemMemory.ramGb > 0 && model.minRamGb > systemMemory.ramGb;
                 const disabled = lacksRam;
                 const selected = settings.model === model.key;
                 const isDownloading = downloading === model.key;
                 const progress = isDownloading && downloadProgress?.model === model.key ? downloadProgress : undefined;
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
                      disabled={disabled}
                      onChange={() => void save({ ...settings, model: model.key })}
                    />
                     <span className="model-option__identity"><ProviderMark provider={model.provider} /><span className="model-option__name"><strong title={model.display}>{model.display}</strong><small>{model.provider} · {model.languages} · {installed ? formatBytes(model.installedSizeBytes) : `~${formatBytes(model.estimatedSizeBytes)}`}</small></span></span>
                     <span className="model-option__meta">
                       {selected ? <span className="model-option__check" aria-hidden="true"><Check size={15} /></span> : null}
                       <span className={`model-option__status model-option__status--${model.status}`}><i aria-hidden="true" />{model.source === "cloud" ? `${t("settings.models.source.cloud")} · ` : ""}{model.status === "ready" ? t("settings.models.status.downloaded") : model.status === "error" ? t("common.error") : t("settings.models.status.notDownloaded")}</span>
                        {!installed && !disabled ? <button type="button" className={`model-download ${isDownloading ? "model-download--active" : ""}`} disabled={Boolean(downloading) || Boolean(deleting)} onClick={(event) => { event.preventDefault(); void download(model.key); }} aria-label={t("settings.models.downloadAria", { model: model.display })}>{isDownloading ? progress?.phase === "validating" ? t("settings.models.checking") : progressPercent === null ? t("settings.models.preparingShort") : `${progressPercent}%` : t("settings.models.download")}</button> : null}
                       {installed && !selected ? <button type="button" className="model-remove" disabled={Boolean(downloading) || Boolean(deleting)} onClick={(event) => { event.preventDefault(); void remove(model); }} aria-label={t("settings.models.removeAria", { model: model.display })} title={t("settings.models.removeTitle")}>{isDeleting ? "…" : <Trash2 size={14} />}</button> : null}
                     </span>
                     {progress ? <span className="model-download-progress" aria-label={progressPercent === null ? t("settings.models.progress.preparing") : t("settings.models.progress.downloaded", { percent: progressPercent })}><span style={{ width: `${progressPercent ?? 4}%` }} /></span> : null}
                   </label>
                 );
               })}
             </div>
             <div className="model-meta"><span>{t("settings.models.active")} <strong>{selectedModel?.display ?? t("settings.models.selecting")}</strong></span><span>{t("settings.models.requires")} <strong>{selectedModel ? `${selectedModel.minVramGb} GB VRAM · ${selectedModel.minRamGb} GB RAM` : t("settings.models.detecting")}</strong></span><span>{t("settings.models.thisComputer")} <strong>{systemMemory.cpuCores ? t("settings.models.cores", { cores: systemMemory.cpuCores }) : ""}{systemMemory.ramGb ? `${systemMemory.ramGb} GB RAM` : t("settings.models.ramUnknown")}</strong></span></div>
             {(modelStatus?.message ?? selectedModel?.message) && selectedState !== "ready" ? <p className="model-card__message">{modelStatus?.message ?? selectedModel?.message}</p> : null}
           </section>
           <p className="privacy-line"><strong>Offline by default</strong><span>{t("settings.privacy.body")}</span></p>
      </div>
    </section>
  );
}
