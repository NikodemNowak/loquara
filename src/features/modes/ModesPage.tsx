import { useEffect, useState } from "react";

import { Plus, Trash2 } from "../../components/Icons";
import type { AppAdapter } from "../../lib/tauri";
import type { Mode } from "../../lib/types";

const blankMode = (): Mode => ({
  id: `custom-${Date.now()}`,
  name: "",
  description: "",
  prompt: "",
  enabled: true,
  isDefault: false,
  createdAt: Date.now(),
});

export function ModesPage({ adapter }: { adapter: AppAdapter }) {
  const [modes, setModes] = useState<Mode[]>([]);
  const [draft, setDraft] = useState<Mode>();
  useEffect(() => {
    void adapter.listModes().then((items) => {
      setModes(items);
      setDraft(items[0]);
    });
  }, [adapter]);

  const save = async () => {
    if (!draft?.name.trim()) return;
    await adapter.upsertMode(draft);
    setModes((current) => current.some(({ id }) => id === draft.id)
      ? current.map((item) => item.id === draft.id ? draft : item)
      : [...current, draft]);
  };
  const remove = async () => {
    if (!draft || draft.isDefault) return;
    if (await adapter.deleteMode(draft.id)) {
      const next = modes.filter(({ id }) => id !== draft.id);
      setModes(next);
      setDraft(next[0]);
    }
  };

  return (
    <section className="page modes-page">
      <header className="page-header"><div><p className="eyebrow">Styl wypowiedzi</p><h1>Tryby</h1><p>Wybierz sposób porządkowania tekstu po transkrypcji.</p></div><button className="primary-button" onClick={() => setDraft(blankMode())}><Plus size={16} />Nowy tryb</button></header>
      <div className="modes-layout">
        <nav className="mode-list" aria-label="Lista trybów">
          {modes.map((mode) => <button key={mode.id} className={draft?.id === mode.id ? "mode-item mode-item--selected" : "mode-item"} onClick={() => setDraft(mode)} aria-label={`${mode.name}, ${mode.description}`}><span><strong>{mode.name}</strong>{mode.isDefault && <em>Wbudowany</em>}</span><small>{mode.description}</small></button>)}
        </nav>
        <form className="mode-editor" onSubmit={(event) => { event.preventDefault(); void save(); }}>
          {draft ? <>
            <div className="editor-heading"><div><span className="eyebrow">{draft.isDefault ? "Tryb wbudowany" : "Tryb własny"}</span><h2>{draft.name || "Nowy tryb"}</h2></div><label className="switch-label">Aktywny <input type="checkbox" checked={draft.enabled} onChange={(event) => setDraft({ ...draft, enabled: event.target.checked })} /></label></div>
            <label><span>Nazwa</span><input value={draft.name} onChange={(event) => setDraft({ ...draft, name: event.target.value })} /></label>
            <label><span>Opis</span><input value={draft.description} onChange={(event) => setDraft({ ...draft, description: event.target.value })} /></label>
            <label><span>Instrukcja</span><textarea rows={7} value={draft.prompt} onChange={(event) => setDraft({ ...draft, prompt: event.target.value })} placeholder="Opisz, jak ma wyglądać wynik…" /></label>
            <p className="field-hint">Instrukcja działa lokalnie po rozpoznaniu mowy. Pisz krótko i konkretnie.</p>
            <div className="editor-actions"><button type="button" className="danger-button" disabled={draft.isDefault} onClick={() => void remove()}><Trash2 size={15} />Usuń tryb</button><button className="primary-button" disabled={!draft.name.trim()} type="submit">Zapisz tryb</button></div>
          </> : <p>Wybierz tryb z listy.</p>}
        </form>
      </div>
    </section>
  );
}
