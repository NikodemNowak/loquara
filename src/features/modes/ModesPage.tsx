import { useCallback, useEffect, useState } from "react";

import { EmptyState } from "../../components/EmptyState";
import { Plus, Trash2 } from "../../components/Icons";
import type { ToastKind } from "../../components/Toast";
import type { AppAdapter } from "../../lib/tauri";
import type { AppSettings, Mode } from "../../lib/types";
import { normalizeError } from "../../lib/errors";
import { useI18n } from "../../lib/i18n";

const BUILTIN_IDS = new Set(["clean", "message", "code"]);

const blankMode = (): Mode => ({
  id: `custom-${Date.now()}`,
  name: "",
  description: "",
  prompt: "",
  enabled: true,
  isDefault: false,
  createdAt: Date.now(),
});

export function ModesPage({
  adapter,
  settings,
  onSettingsChange,
  onToast,
}: {
  adapter: AppAdapter;
  settings: AppSettings;
  onSettingsChange: (settings: AppSettings) => void;
  onToast: (message: string, kind: ToastKind) => void;
}) {
  const { t } = useI18n();
  const [modes, setModes] = useState<Mode[]>([]);
  const [draft, setDraft] = useState<Mode>();
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState("");
  const [busy, setBusy] = useState("");

  const load = useCallback(async () => {
    setLoading(true);
    setLoadError("");
    try {
      const items = await adapter.listModes();
      setModes(items);
      setDraft((current) => items.find(({ id }) => id === current?.id) ?? items[0]);
    } catch (error) {
      setLoadError(t("modes.loadError", { error: normalizeError(error) }));
    } finally {
      setLoading(false);
    }
  }, [adapter, t]);

  useEffect(() => { void load(); }, [load]);

  const run = async (key: string, action: () => Promise<void>) => {
    setBusy(key);
    try {
      await action();
    } catch (error) {
      onToast(t("common.error.action", { error: normalizeError(error) }), "error");
    } finally {
      setBusy("");
    }
  };

  const save = async () => {
    if (!draft?.name.trim()) return;
    await run("save", async () => {
      await adapter.upsertMode(draft);
      setModes((current) => current.some(({ id }) => id === draft.id)
        ? current.map((item) => item.id === draft.id ? draft : item)
        : [...current, draft]);
    });
  };

  const remove = async () => {
    if (!draft || BUILTIN_IDS.has(draft.id)) return;
    await run("delete", async () => {
      if (!await adapter.deleteMode(draft.id)) return;
      const next = modes.filter(({ id }) => id !== draft.id);
      setModes(next);
      setDraft(next[0]);
    });
  };

  const useMode = async () => {
    if (!draft?.enabled || draft.id === settings.activeMode) return;
    await run("use", async () => {
      const next = { ...settings, activeMode: draft.id };
      const result = await adapter.updateSettings(next);
      const persisted = await adapter.getSettings().catch(() => result.settings);
      onSettingsChange(persisted);
      if (result.warning) onToast(result.warning, "info");
    });
  };

  return (
    <section className="page modes-page">
      <header className="page-header">
        <div><p className="eyebrow">{t("modes.eyebrow")}</p><h1>{t("modes.title")}</h1><p>{t("modes.subtitle")}</p></div>
        <button className="primary-button" disabled={loading} onClick={() => setDraft(blankMode())}><Plus size={16} />{t("modes.new")}</button>
      </header>
      {loading ? <div className="panel-loading" role="status">{t("modes.loading")}</div> :
        loadError ? <EmptyState title={t("modes.loadTitle")} description={loadError} action={<button className="primary-button" onClick={() => void load()}>{t("common.retryAction")}</button>} /> :
        <div className="modes-layout">
          <nav className="mode-list" aria-label={t("modes.list.label")}>
            {modes.map((mode) => (
              <button key={mode.id} className={draft?.id === mode.id ? "mode-item mode-item--selected" : "mode-item"} onClick={() => setDraft(mode)} aria-label={t("modes.list.aria", { name: mode.name, description: mode.description })}>
                <span><strong>{mode.name}</strong>{settings.activeMode === mode.id && <em>{t("modes.inUse")}</em>}</span>
                <small>{mode.description}</small>
              </button>
            ))}
          </nav>
          <form className="mode-editor" onSubmit={(event) => { event.preventDefault(); void save(); }}>
            {draft ? <>
              <div className="editor-heading">
                <div><span className="eyebrow">{BUILTIN_IDS.has(draft.id) ? t("modes.editor.builtin") : t("modes.editor.custom")}</span><h2>{draft.name || t("modes.editor.newTitle")}</h2></div>
                <label className="switch-label">{t("modes.editor.enabled")} <input type="checkbox" checked={draft.enabled} onChange={(event) => setDraft({ ...draft, enabled: event.target.checked })} /></label>
              </div>
              <label><span>{t("modes.editor.name")}</span><input value={draft.name} onChange={(event) => setDraft({ ...draft, name: event.target.value })} /></label>
              <label><span>{t("modes.editor.description")}</span><input value={draft.description} onChange={(event) => setDraft({ ...draft, description: event.target.value })} /></label>
              <label><span>{t("modes.editor.prompt")}</span><textarea rows={7} value={draft.prompt} onChange={(event) => setDraft({ ...draft, prompt: event.target.value })} placeholder={t("modes.editor.promptPlaceholder")} /></label>
              <p className="field-hint">{t("modes.editor.hint.intro")} <code>case: lower|upper|sentence</code>, <code>layout: bullets|plain</code>, <code>prefix: …</code>, <code>suffix: …</code>. {t("modes.editor.hint.outro")}</p>
              <div className="editor-actions">
                <button type="button" className="danger-button" disabled={BUILTIN_IDS.has(draft.id) || settings.activeMode === draft.id || Boolean(busy)} onClick={() => void remove()}><Trash2 size={15} />{busy === "delete" ? t("common.deleting") : t("modes.editor.delete")}</button>
                <span className="editor-actions__right">
                  <button type="button" disabled={!draft.enabled || settings.activeMode === draft.id || Boolean(busy)} onClick={() => void useMode()}>{busy === "use" ? t("modes.editor.activating") : settings.activeMode === draft.id ? t("modes.inUse") : t("modes.editor.use")}</button>
                  <button className="primary-button" disabled={!draft.name.trim() || Boolean(busy)} type="submit">{busy === "save" ? t("modes.editor.saving") : t("modes.editor.save")}</button>
                </span>
              </div>
            </> : <EmptyState title={t("modes.empty.title")} description={t("modes.empty.description")} />}
          </form>
        </div>}
    </section>
  );
}
