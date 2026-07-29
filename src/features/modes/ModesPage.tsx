import { useCallback, useEffect, useState } from "react";

import { EmptyState } from "../../components/EmptyState";
import { Plus, Trash2 } from "../../components/Icons";
import type { ToastKind } from "../../components/Toast";
import type { AppAdapter } from "../../lib/tauri";
import type { AppSettings, Mode } from "../../lib/types";
import { normalizeError } from "../../lib/errors";

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
      setLoadError(`Nie udało się wczytać trybów: ${normalizeError(error)}`);
    } finally {
      setLoading(false);
    }
  }, [adapter]);

  useEffect(() => { void load(); }, [load]);

  const run = async (key: string, action: () => Promise<void>) => {
    setBusy(key);
    try {
      await action();
    } catch (error) {
      onToast(`Nie udało się wykonać akcji: ${normalizeError(error)}`, "error");
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
        <div><p className="eyebrow">Styl wypowiedzi</p><h1>Tryby</h1><p>Wybierz sposób porządkowania tekstu po transkrypcji.</p></div>
        <button className="primary-button" disabled={loading} onClick={() => setDraft(blankMode())}><Plus size={16} />Nowy tryb</button>
      </header>
      {loading ? <div className="panel-loading" role="status">Wczytuję tryby…</div> :
        loadError ? <EmptyState title="Nie udało się wczytać trybów" description={loadError} action={<button className="primary-button" onClick={() => void load()}>Spróbuj ponownie</button>} /> :
        <div className="modes-layout">
          <nav className="mode-list" aria-label="Lista trybów">
            {modes.map((mode) => (
              <button key={mode.id} className={draft?.id === mode.id ? "mode-item mode-item--selected" : "mode-item"} onClick={() => setDraft(mode)} aria-label={`${mode.name}, ${mode.description}`}>
                <span><strong>{mode.name}</strong>{settings.activeMode === mode.id && <em>Używany</em>}</span>
                <small>{mode.description}</small>
              </button>
            ))}
          </nav>
          <form className="mode-editor" onSubmit={(event) => { event.preventDefault(); void save(); }}>
            {draft ? <>
              <div className="editor-heading">
                <div><span className="eyebrow">{BUILTIN_IDS.has(draft.id) ? "Tryb wbudowany" : "Tryb własny"}</span><h2>{draft.name || "Nowy tryb"}</h2></div>
                <label className="switch-label">Aktywny <input type="checkbox" checked={draft.enabled} onChange={(event) => setDraft({ ...draft, enabled: event.target.checked })} /></label>
              </div>
              <label><span>Nazwa</span><input value={draft.name} onChange={(event) => setDraft({ ...draft, name: event.target.value })} /></label>
              <label><span>Opis</span><input value={draft.description} onChange={(event) => setDraft({ ...draft, description: event.target.value })} /></label>
              <label><span>Instrukcja</span><textarea rows={7} value={draft.prompt} onChange={(event) => setDraft({ ...draft, prompt: event.target.value })} placeholder={"case: lower\nlayout: bullets\nprefix: Notatka"} /></label>
              <p className="field-hint">Własne tryby używają lokalnych dyrektyw: <code>case: lower|upper|sentence</code>, <code>layout: bullets|plain</code>, <code>prefix: …</code>, <code>suffix: …</code>. Pozostały tekst jest ignorowany, a wynik używa porządkowania Czysty.</p>
              <div className="editor-actions">
                <button type="button" className="danger-button" disabled={BUILTIN_IDS.has(draft.id) || settings.activeMode === draft.id || Boolean(busy)} onClick={() => void remove()}><Trash2 size={15} />{busy === "delete" ? "Usuwam…" : "Usuń tryb"}</button>
                <span className="editor-actions__right">
                  <button type="button" disabled={!draft.enabled || settings.activeMode === draft.id || Boolean(busy)} onClick={() => void useMode()}>{busy === "use" ? "Włączam…" : settings.activeMode === draft.id ? "Używany" : "Użyj"}</button>
                  <button className="primary-button" disabled={!draft.name.trim() || Boolean(busy)} type="submit">{busy === "save" ? "Zapisuję…" : "Zapisz tryb"}</button>
                </span>
              </div>
            </> : <EmptyState title="Brak trybów" description="Dodaj własny tryb, aby rozpocząć." />}
          </form>
        </div>}
    </section>
  );
}
