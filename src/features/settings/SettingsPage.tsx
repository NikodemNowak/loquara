import { useEffect, useRef, useState } from "react";

import { Check, Cpu, FolderOpen, Languages, Mic, SlidersHorizontal, Trash2 } from "../../components/Icons";
import { BrandLogo } from "../../components/BrandLogo";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { ShortcutCapture } from "./ShortcutCapture";
import { Select } from "../../components/Select";
import { HuggingFaceGate } from "./HuggingFaceGate";
import type { ToastKind } from "../../components/Toast";
import type { AppAdapter } from "../../lib/tauri";
import type { AppSettings, HfAccount, InputDeviceInfo, ModelDescriptor, ModelDownloadProgress, ModelStatus } from "../../lib/types";
import { normalizeError } from "../../lib/errors";
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
 * selected", so an option carrying it renders blank. Device ids are the
 * operating system's device names, which never contain a colon-prefixed
 * namespace, so this cannot collide with a real device.
 */
const SYSTEM_DEFAULT_DEVICE = "loquara:system-default";

/** Whether a failed download was refused for lack of Hugging Face access. */
function isAccessFailure(error: unknown): boolean {
  const raw = typeof error === "string" ? error : String((error as { message?: string })?.message ?? error);
  return raw.includes("requires accepting its licence on Hugging Face")
    || raw.includes("rejected the access token for");
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
  const [downloading, setDownloading] = useState("");
  const [deleting, setDeleting] = useState("");
  const [pendingDelete, setPendingDelete] = useState<ModelDescriptor>();
  const [downloadProgress, setDownloadProgress] = useState<ModelDownloadProgress>();
  /** The model whose download stopped because it needs Hugging Face access. */
  const [gatedModel, setGatedModel] = useState<ModelDescriptor>();
  const [hfAccount, setHfAccount] = useState<HfAccount>({ connected: false, name: null });

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
    void adapter.hfAccount()
      .then((account) => { if (active) setHfAccount(account); })
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
    const next = { ...previous, ...patch };
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
      const message = normalizeError(error);
      // Access failures are not really errors to dismiss: there are steps the
      // user can take, so the gate explains them next to the model itself.
      if (isAccessFailure(error)) {
        setGatedModel(models.find((candidate) => candidate.key === key));
      } else {
        onToast(t("settings.models.downloadError", { error: message }), "error");
      }
    } finally {
      setDownloading("");
      setDownloadProgress(undefined);
    }
  };

  const remove = async (model: ModelDescriptor) => {
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
      <header className="page-header">
        <h1>{t("settings.title")}</h1>
        <p>{t("settings.subtitle")}</p>
      </header>

      <div className="settings-column">
        <section className="settings-group">
          <div className="group-heading">
            <h2>{t("settings.recording.title")}</h2>
          </div>
          <label className="setting-row">
            <span><strong>{t("settings.mic.label")}</strong><small>{deviceError || t("settings.mic.description")}</small></span>
            <Select
              label={t("settings.mic.label")}
              disabled={Boolean(deviceError)}
              value={settings.inputDevice ?? SYSTEM_DEFAULT_DEVICE}
              onChange={(next) => void save({ inputDevice: next === SYSTEM_DEFAULT_DEVICE ? null : next })}
              options={[
                { value: SYSTEM_DEFAULT_DEVICE, label: t("settings.mic.default") },
                ...devices.map((device) => ({ value: device.id, label: device.name })),
              ]}
            />
          </label>
          <div className="setting-row">
            <span><strong>{t("settings.shortcut.label")}</strong><small>{t("settings.shortcut.description")}</small></span>
            <ShortcutCapture value={settings.shortcut} onCapture={(combo) => void save({ shortcut: combo })} onActiveChange={(active) => { void adapter.setShortcutSuspended(active).catch(() => undefined); }} />
          </div>
        </section>

        <section className="settings-group">
          <div className="group-heading">
            <h2>{t("settings.behavior.title")}</h2>
          </div>
          <label className="setting-row">
            <span><strong>{t("settings.autoPaste.label")}</strong><small>{t("settings.autoPaste.description")}</small></span>
            <input type="checkbox" checked={settings.autoPaste} aria-label={t("settings.autoPaste.label")} onChange={(event) => void save({ autoPaste: event.target.checked })} />
          </label>
          <label className="setting-row">
            <span><strong>{t("settings.showOverlay.label")}</strong><small>{t("settings.showOverlay.description")}</small></span>
            <input type="checkbox" checked={settings.showOverlay} aria-label={t("settings.showOverlay.label")} onChange={(event) => void save({ showOverlay: event.target.checked })} />
          </label>
          <label className="setting-row">
            <span><strong>{t("settings.launchOnLogin.label")}</strong><small>{t("settings.launchOnLogin.description")}</small></span>
            <input type="checkbox" checked={settings.launchOnLogin} aria-label={t("settings.launchOnLogin.label")} onChange={(event) => void save({ launchOnLogin: event.target.checked })} />
          </label>
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
          <label className="setting-row">
            <span><strong>{t("settings.dictationLanguage.label")}</strong><small>{t("settings.dictationLanguage.description")}</small></span>
            <Select
              label={t("settings.dictationLanguage.label")}
              value={settings.dictationLanguage}
              onChange={(next) => void save({ dictationLanguage: next })}
              options={[
                { value: "auto", label: t("settings.dictationLanguage.auto") },
                { value: "pl", label: t("settings.dictationLanguage.pl") },
                { value: "en", label: t("settings.dictationLanguage.en") },
              ]}
            />
          </label>
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
        </section>

        <section className="settings-group" aria-busy={!models.length}>
          <div className="group-heading group-heading--split">
            <div>
              <h2>{t("settings.models.heading")}</h2>
              <p>{t("settings.models.intro")}</p>
            </div>
            <span className={`group-status group-status--${selectedState ?? "checking"}`}>
              {selectedState === "ready" && <Check size={13} />}
              {selectedStatusLabel}
            </span>
          </div>
          <div className="model-options" role="radiogroup" aria-label={t("settings.models.heading")}>
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
                      {model.source === "cloud" ? `${t("settings.models.source.cloud")} · ` : ""}
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
          {gatedModel && (
            <HuggingFaceGate
              adapter={adapter}
              model={gatedModel}
              account={hfAccount}
              onAccount={setHfAccount}
            />
          )}
          {(modelStatus?.message ?? selectedModel?.message) && selectedState !== "ready"
            ? <p className="model-card__message">{modelStatus?.message ?? selectedModel?.message}</p>
            : null}
        </section>

        <section className="settings-group">
          <div className="group-heading">
            <h2>{t("settings.general.title")}</h2>
            <p>{t("settings.general.subtitle")}</p>
          </div>
          <div className="setting-block">
            <strong>{t("settings.language.label")}</strong>
            <div className="segment" role="radiogroup" aria-label={t("settings.language.label")}>
              {([["system", "settings.language.system"], ["pl", "settings.language.pl"], ["en", "settings.language.en"]] as const).map(([value, label]) => (
                <label key={value}>
                  <input type="radio" name="language" checked={settings.language === value} onChange={() => void save({ language: value })} />
                  <span>{t(label)}</span>
                </label>
              ))}
            </div>
          </div>
        </section>

        <p className="privacy-line">
          <strong>{t("settings.privacy.title")}</strong>
          <span>{t("settings.privacy.body")}</span>
        </p>
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
