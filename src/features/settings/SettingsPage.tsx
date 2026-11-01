import { useEffect, useMemo, useRef, useState } from "react";

import { Check, Trash2 } from "../../components/Icons";
import { Logo } from "../../components/Logo";
import "./SettingsPage.css";
import { BrandLogo } from "../../components/BrandLogo";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { ShortcutCapture } from "./ShortcutCapture";
import { Select } from "../../components/Select";
import type { ToastKind } from "../../components/Toast";
import type { AppAdapter } from "../../lib/tauri";
import type { AppSettings, CloudProviderInfo, DownloadStatus, InputDeviceInfo, ModelDescriptor, ModelDownloadProgress, ModelStatus, PasteMode, TranscriptionSource } from "../../lib/types";
import { realtimeEnabled, realtimeEngineKey } from "../../lib/types";
import { microphoneChoices, microphonePreferences } from "../../lib/microphones";
import { normalizeError } from "../../lib/errors";
import { formatBytes as formatSize } from "../../lib/bytes";
import { useI18n, type TranslationKey } from "../../lib/i18n";

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

/**
 * Languages the cloud engines take as a hint.
 *
 * Short on purpose: this only nudges formatting and punctuation, and every
 * provider accepts "auto" when the speaker is somewhere else entirely.
 */
const CLOUD_LANGUAGES = ["auto", "pl", "en", "de", "fr", "es", "it"] as const;

const CLOUD_LANGUAGE_LABELS: Record<(typeof CLOUD_LANGUAGES)[number], TranslationKey> = {
  auto: "settings.cloud.language.auto",
  pl: "settings.cloud.language.pl",
  en: "settings.cloud.language.en",
  de: "settings.cloud.language.de",
  fr: "settings.cloud.language.fr",
  es: "settings.cloud.language.es",
  it: "settings.cloud.language.it",
};

/**
 * Stands for "write the model id myself" in the model dropdown.
 *
 * The empty string cannot be used: the dropdown reserves it to mean "no value
 * selected", so an option carrying it renders blank.
 */
const CUSTOM_MODEL = "loquara:custom-model";

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
  const [tab, setTab] = useState<"dictation" | "models" | "app">("dictation");
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
  const [providers, setProviders] = useState<CloudProviderInfo[]>([]);
  const [providersLoading, setProvidersLoading] = useState(true);
  const [providersError, setProvidersError] = useState("");
  const [providersRetry, setProvidersRetry] = useState(0);
  const [cloudKeys, setCloudKeys] = useState<string[]>([]);
  const [keysLoading, setKeysLoading] = useState(true);
  const [keysError, setKeysError] = useState("");
  const [keysRetry, setKeysRetry] = useState(0);
  const [apiKeyDraft, setApiKeyDraft] = useState("");
  const [keyBusy, setKeyBusy] = useState(false);
  const [testing, setTesting] = useState(false);
  // The model and address fields write on blur rather than on every key: a
  // save per character would rewrite the database while the user types.
  const [modelDraft, setModelDraft] = useState("");
  const [baseUrlDraft, setBaseUrlDraft] = useState("");
  const [fetchedModels, setFetchedModels] = useState<string[]>([]);
  const [fetchingModels, setFetchingModels] = useState(false);
  const [fetchModelsMessage, setFetchModelsMessage] = useState("");
  const [fetchModelsFailed, setFetchModelsFailed] = useState(false);
  const modelFetchAttempt = useRef(0);
  const [customModel, setCustomModel] = useState(false);

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
    setProvidersLoading(true);
    setProvidersError("");
    void adapter.listCloudProviders()
      .then((loaded) => { if (active) setProviders(loaded); })
      .catch((error) => { if (active) setProvidersError(normalizeError(error)); })
      .finally(() => { if (active) setProvidersLoading(false); });
    return () => { active = false; };
  }, [adapter, providersRetry]);

  useEffect(() => {
    let active = true;
    setKeysLoading(true);
    setKeysError("");
    void adapter.listCloudKeys()
      .then((loaded) => { if (active) setCloudKeys(loaded); })
      .catch((error) => { if (active) setKeysError(normalizeError(error)); })
      .finally(() => { if (active) setKeysLoading(false); });
    return () => { active = false; };
  }, [adapter, keysRetry]);

  useEffect(() => {
    modelFetchAttempt.current += 1;
    setFetchedModels([]);
    setFetchingModels(false);
    setFetchModelsMessage("");
    setFetchModelsFailed(false);
    return () => { modelFetchAttempt.current += 1; };
  }, [settings.cloudProvider, settings.cloudBaseUrl, adapter]);

  // Credentials belong to the provider; changing its endpoint must preserve
  // a key the user has typed but has not explicitly saved yet.
  useEffect(() => {
    setApiKeyDraft("");
  }, [settings.cloudProvider, adapter]);

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

  const source: TranscriptionSource = settings.transcriptionSource ?? "local";
  const provider = providers.find((item) => item.key === settings.cloudProvider);
  // Empty settings mean "whatever the provider calls its current model".
  const effectiveModel = settings.cloudModel || provider?.defaultModel || "";
  const realtimeSupported = source === "local" ? settings.model === "parakeet" : Boolean(provider?.streaming);
  const keyStored = cloudKeys.includes(settings.cloudProvider);
  // Presets and anything the provider itself listed, without duplicates.
  const modelOptions = useMemo(
    () => Array.from(new Set([...(provider?.models ?? []), ...fetchedModels])),
    [provider, fetchedModels],
  );

  // A model that is not on the list was typed by hand, so it keeps the text
  // field rather than looking like a value the dropdown lost.
  useEffect(() => {
    if (settings.cloudModel && modelOptions.length && !modelOptions.includes(settings.cloudModel)) {
      setCustomModel(true);
    }
  }, [settings.cloudModel, modelOptions]);

  // The drafts follow the saved values, so a change made elsewhere — or a
  // provider switch that resets the model — lands in the fields too.
  useEffect(() => {
    setModelDraft(effectiveModel);
  }, [effectiveModel]);
  useEffect(() => {
    setBaseUrlDraft(settings.cloudBaseUrl);
  }, [settings.cloudBaseUrl]);

  const changeProvider = (key: string) => {
    const chosen = providers.find((item) => item.key === key);
    modelFetchAttempt.current += 1;
    setFetchedModels([]);
    setCustomModel(false);
    void save({
      cloudProvider: key,
      cloudModel: chosen?.defaultModel ?? "",
      cloudBaseUrl: "",
    });
  };

  const fetchModels = async () => {
    const attempt = ++modelFetchAttempt.current;
    const owner = { provider: latest.current.cloudProvider, baseUrl: latest.current.cloudBaseUrl };
    const isCurrent = () => attempt === modelFetchAttempt.current
      && owner.provider === latest.current.cloudProvider
      && owner.baseUrl === latest.current.cloudBaseUrl;
    setFetchingModels(true);
    setFetchModelsMessage("");
    setFetchModelsFailed(false);
    try {
      const models = await adapter.listCloudModels();
      if (!isCurrent()) return;
      setFetchedModels(models);
      if (!models.length) {
        setFetchModelsMessage(t("settings.cloud.model.fetchEmpty"));
        onToast(t("settings.cloud.model.fetchEmpty"), "info");
        return;
      }
      onToast(t("settings.cloud.model.fetched", { count: models.length }), "success");
    } catch (error) {
      if (!isCurrent()) return;
      const message = t("settings.cloud.model.fetchError", { error: normalizeError(error) });
      setFetchModelsMessage(message);
      setFetchModelsFailed(true);
      onToast(message, "error");
    } finally {
      if (isCurrent()) setFetchingModels(false);
    }
  };

  const chooseModel = (next: string) => {
    if (next === CUSTOM_MODEL) {
      setCustomModel(true);
      return;
    }
    setCustomModel(false);
    setModelDraft(next);
    void save({ cloudModel: next });
  };

  const commitModel = () => {
    const next = modelDraft.trim();
    if (next !== settings.cloudModel) void save({ cloudModel: next });
  };

  const commitBaseUrl = () => {
    const next = baseUrlDraft.trim();
    if (next !== settings.cloudBaseUrl) void save({ cloudBaseUrl: next });
  };

  const saveApiKey = async () => {
    if (!apiKeyDraft.trim()) return;
    const owner = settings.cloudProvider;
    setKeyBusy(true);
    try {
      setCloudKeys(await adapter.setCloudApiKey(owner, apiKeyDraft));
      setKeysError("");
      if (latest.current.cloudProvider === owner) setApiKeyDraft("");
      onToast(t("settings.cloud.key.saved"), "success");
    } catch (error) {
      onToast(t("settings.cloud.key.error", { error: normalizeError(error) }), "error");
    } finally {
      setKeyBusy(false);
    }
  };

  const clearApiKey = async () => {
    const owner = settings.cloudProvider;
    setKeyBusy(true);
    try {
      setCloudKeys(await adapter.clearCloudApiKey(owner));
      setKeysError("");
      if (latest.current.cloudProvider === owner) setApiKeyDraft("");
      onToast(t("settings.cloud.key.cleared"), "info");
    } catch (error) {
      onToast(t("settings.cloud.key.error", { error: normalizeError(error) }), "error");
    } finally {
      setKeyBusy(false);
    }
  };

  const testCloud = async () => {
    setTesting(true);
    try {
      const answer = await adapter.testCloudTranscription();
      const seconds = (answer.elapsedMs / 1000).toFixed(1);
      onToast(
        answer.text.trim()
          ? t("settings.cloud.testOk", { seconds, text: answer.text.trim() })
          : t("settings.cloud.testOkEmpty", { seconds }),
        "success",
      );
    } catch (error) {
      onToast(t("settings.cloud.testError", { error: normalizeError(error) }), "error");
    } finally {
      setTesting(false);
    }
  };

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
        {(["dictation", "models", "app"] as const).map((item) => <button type="button" key={item} id={`settings-tab-${item}`}
          role="tab" aria-selected={tab === item} aria-controls={`settings-panel-${item}`} tabIndex={tab === item ? 0 : -1}
          onClick={() => setTab(item)} onKeyDown={(event) => {
            const items = ["dictation", "models", "app"] as const;
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
          <div className="settings-source segment" role="radiogroup" aria-label={t("settings.cloud.source.label")}>
            {([["local", "settings.cloud.source.local"], ["cloud", "settings.cloud.source.cloud"]] as const).map(([value, label]) => (
              <label key={value}>
                <input type="radio" name="transcription-source" aria-label={t(label)} aria-describedby="settings-source-privacy" checked={source === value} onChange={() => void save({ transcriptionSource: value })} />
                <span>{t(label)}</span>
              </label>
            ))}
          </div>
          <p className="settings-source-privacy" id="settings-source-privacy">
            {source === "local" ? t("settings.privacy.body") : t("settings.cloud.privacy", { provider: provider?.display ?? settings.cloudProvider })}
          </p>
          {source === "cloud" ? (
            <div className="cloud-panel">
              {providersLoading ? <p className="settings-feedback" role="status">{t("settings.models.checking")}</p> : providersError || !providers.length ? <div className={`settings-feedback ${providersError ? "settings-feedback--error" : ""}`} role={providersError ? "alert" : "status"}><span>{providersError ? t("settings.cloud.providers.error", { error: providersError }) : t("settings.cloud.providers.empty")}</span><button className="text-button" type="button" onClick={() => setProvidersRetry((value) => value + 1)}>{t("settings.retry")}</button></div> : null}
              <label className="setting-row cloud-provider-row">
                <span><strong>{t("settings.cloud.provider.label")}</strong><small>{t("settings.cloud.provider.description")}</small></span>
                <Select
                  label={t("settings.cloud.provider.label")}
                  value={settings.cloudProvider}
                  onChange={changeProvider}
                  disabled={providersLoading || Boolean(providersError) || !providers.length}
                  options={providers.map((item) => ({ value: item.key, label: item.display }))}
                />
              </label>
              <div className="setting-row cloud-model-row">
                <span><strong>{t("settings.cloud.model.label")}</strong><small>{t("settings.cloud.model.description")}</small></span>
                <div className="cloud-model">
                  {customModel ? (
                    <input
                      value={modelDraft}
                      placeholder={provider?.defaultModel || t("settings.cloud.model.placeholder")}
                      aria-label={t("settings.cloud.model.label")}
                      onChange={(event) => setModelDraft(event.target.value)}
                      onBlur={commitModel}
                      onKeyDown={(event) => { if (event.key === "Enter") event.currentTarget.blur(); }}
                    />
                  ) : (
                    <Select
                      label={t("settings.cloud.model.label")}
                      value={effectiveModel}
                      onChange={chooseModel}
                      options={[
                        ...modelOptions.map((model) => ({ value: model, label: model })),
                        { value: CUSTOM_MODEL, label: t("settings.cloud.model.custom") },
                      ]}
                    />
                  )}
                  <div className="cloud-model__actions">
                    {provider?.listsModels ? (
                      <button
                        type="button"
                        className="text-button"
                        disabled={fetchingModels || keysLoading || Boolean(keysError) || !keyStored}
                        onClick={() => void fetchModels()}
                      >
                        {fetchingModels ? <><Logo size={15} motion="loading" />{t("settings.cloud.model.fetching")}</> : t(fetchModelsFailed ? "settings.retry" : "settings.cloud.model.fetch")}
                      </button>
                    ) : null}
                    {customModel ? (
                      <button type="button" className="text-button" onClick={() => setCustomModel(false)}>
                        {t("settings.cloud.model.pick")}
                      </button>
                    ) : null}
                  </div>
                  {fetchModelsMessage ? <small className={fetchModelsFailed ? "settings-feedback--error" : ""} role={fetchModelsFailed ? "alert" : "status"}>{fetchModelsMessage}</small> : null}
                </div>
              </div>
              {provider?.custom ? (
                <label className="setting-row">
                  <span><strong>{t("settings.cloud.baseUrl.label")}</strong><small>{t("settings.cloud.baseUrl.description")}</small></span>
                  <input
                    value={baseUrlDraft}
                    placeholder="https://api.example.com/v1"
                    aria-label={t("settings.cloud.baseUrl.label")}
                    onChange={(event) => setBaseUrlDraft(event.target.value)}
                    onBlur={commitBaseUrl}
                    onKeyDown={(event) => { if (event.key === "Enter") event.currentTarget.blur(); }}
                  />
                </label>
              ) : null}
              <label className="setting-row cloud-language-row">
                <span><strong>{t("settings.cloud.language.label")}</strong><small>{t("settings.cloud.language.description")}</small></span>
                <Select
                  label={t("settings.cloud.language.label")}
                  value={settings.cloudLanguage || "auto"}
                  onChange={(next) => void save({ cloudLanguage: next === "auto" ? "" : next })}
                  options={CLOUD_LANGUAGES.map((code) => ({ value: code, label: t(CLOUD_LANGUAGE_LABELS[code]) }))}
                />
              </label>
              <div className="setting-row cloud-key-row">
                <span>
                  <strong>{t("settings.cloud.key.label")}</strong>
                  <small>{keysLoading ? t("settings.models.checking") : !keysError ? keyStored ? t("settings.cloud.key.present") : t("settings.cloud.key.description") : null}</small>
                </span>
                <div className="cloud-key">
                  <input
                    type="password"
                    value={apiKeyDraft}
                    placeholder={t("settings.cloud.key.placeholder")}
                    aria-label={t("settings.cloud.key.label")}
                    onChange={(event) => setApiKeyDraft(event.target.value)}
                    onKeyDown={(event) => { if (event.key === "Enter") void saveApiKey(); }}
                  />
                  <button
                    type="button"
                    className="secondary-button"
                    disabled={keyBusy || !apiKeyDraft.trim()}
                    onClick={() => void saveApiKey()}
                  >
                    {keyBusy ? <><Logo size={15} motion="loading" />{t("settings.cloud.key.saving")}</> : t("settings.cloud.key.save")}
                  </button>
                  {keyStored ? (
                    <button
                      type="button"
                      className="text-button"
                      disabled={keyBusy}
                      onClick={() => void clearApiKey()}
                    >
                      {t("settings.cloud.key.clear")}
                    </button>
                  ) : null}
                </div>
              </div>
              {keysError ? <div className="settings-feedback settings-feedback--error cloud-keys-error" role="alert"><span>{t("settings.cloud.keys.error", { error: keysError })}</span><button className="text-button" type="button" onClick={() => setKeysRetry((value) => value + 1)}>{t("settings.retry")}</button></div> : null}
              <div className="setting-row cloud-test-row">
                <span><strong>{t("settings.cloud.test.label")}</strong><small>{t("settings.cloud.test.description")}</small></span>
                <button
                  type="button"
                  className="secondary-button"
                  disabled={testing || keysLoading || Boolean(keysError) || !keyStored}
                  onClick={() => void testCloud()}
                >
                  {testing ? <><Logo size={15} motion="loading" />{t("settings.cloud.testing")}</> : t("settings.cloud.test")}
                </button>
              </div>
            </div>
          ) : (
          <>
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
          </>
          )}
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
          <div className="settings-engine">
            <span className={`group-status group-status--${selectedState ?? "checking"}`}>
              {source === "local" && selectedState === "ready" ? <Check size={13} /> : null}
              <span>{source === "cloud" ? effectiveModel || provider?.display || settings.cloudProvider : selectedModel?.display ?? settings.model}</span>
              {source === "local" ? <span className="settings-engine__status">{selectedStatusLabel}</span> : null}
            </span>
            <button type="button" className="text-button" onClick={() => setTab("models")}>{t("settings.manageModels")}</button>
          </div>
          {source === "local" && modelStatus?.message && selectedState !== "ready"
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
          {source === "cloud" ? (
            <>
              <strong>{t("settings.privacy.cloudTitle")}</strong>
              <span>{t("settings.privacy.cloudBody", { provider: provider?.display ?? settings.cloudProvider })}</span>
            </>
          ) : (
            <>
              <strong>{t("settings.privacy.title")}</strong>
              <span>{t("settings.privacy.body")}</span>
            </>
          )}
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
