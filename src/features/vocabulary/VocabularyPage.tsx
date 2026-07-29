import { useEffect, useMemo, useState } from "react";

import { Plus, Search, Trash2 } from "../../components/Icons";
import { EmptyState } from "../../components/EmptyState";
import type { AppAdapter } from "../../lib/tauri";
import type { VocabularyEntry } from "../../lib/types";

export function VocabularyPage({ adapter }: { adapter: AppAdapter }) {
  const [items, setItems] = useState<VocabularyEntry[]>([]);
  const [heard, setHeard] = useState("");
  const [replacement, setReplacement] = useState("");
  const [search, setSearch] = useState("");
  useEffect(() => { void adapter.listVocabulary().then((loaded) => setItems([...loaded])); }, [adapter]);
  const filtered = useMemo(() => items.filter((item) => `${item.heard} ${item.replacement}`.toLocaleLowerCase("pl").includes(search.toLocaleLowerCase("pl"))), [items, search]);

  const add = async () => {
    if (!heard.trim() || !replacement.trim()) return;
    const item = await adapter.addVocabulary(heard.trim(), replacement.trim());
    setItems((current) => [...current.filter(({ id }) => id !== item.id), item]);
    setHeard("");
    setReplacement("");
  };
  const remove = async (item: VocabularyEntry) => {
    if (await adapter.deleteVocabulary(item.id)) setItems((current) => current.filter(({ id }) => id !== item.id));
  };

  return (
    <section className="page vocabulary-page">
      <header className="page-header"><div><p className="eyebrow">Własne nazwy</p><h1>Słownik</h1><p>Naucz Mów nazwisk, marek i terminów używanych w Twojej pracy.</p></div></header>
      <div className="vocabulary-add">
        <label><span>Usłyszane</span><input value={heard} onChange={(event) => setHeard(event.target.value)} placeholder="np. parakit" /></label>
        <span className="replace-arrow" aria-hidden="true">→</span>
        <label><span>Zamień na</span><input value={replacement} onChange={(event) => setReplacement(event.target.value)} placeholder="np. Parakeet" /></label>
        <button className="primary-button" disabled={!heard.trim() || !replacement.trim()} onClick={() => void add()}><Plus size={16} />Dodaj zamianę</button>
      </div>
      <div className="section-heading"><div><h2>Zapisane zamiany</h2><p>{items.length} {items.length === 1 ? "wpis" : "wpisów"}</p></div><label className="search-field search-field--small"><Search size={16} /><span className="sr-only">Szukaj w słowniku</span><input type="search" value={search} onChange={(event) => setSearch(event.target.value)} placeholder="Szukaj" /></label></div>
      <div className="vocabulary-list">
        {filtered.map((item) => <div className="vocabulary-row" key={item.id}><span className="heard-word">{item.heard}</span><span>→</span><strong>{item.replacement}</strong><button className="icon-button" aria-label={`Usuń ${item.heard}`} onClick={() => void remove(item)}><Trash2 size={16} /></button></div>)}
        {!filtered.length && <EmptyState title="Brak pasujących zamian" description="Dodaj pierwszą parę albo zmień wyszukiwanie." />}
      </div>
      <aside className="example-note"><strong>Jak to działa?</strong><p>Gdy powiesz „wyślij to do parakit”, Mów zapisze „wyślij to do Parakeet”. Zamiana działa po zakończeniu transkrypcji.</p></aside>
    </section>
  );
}
