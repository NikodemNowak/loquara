import { useCallback, useEffect, useMemo, useState, type KeyboardEvent } from "react";

import { ChevronDown, Plus, Search, Trash2 } from "../../components/Icons";
import { EmptyState } from "../../components/EmptyState";
import type { ToastKind } from "../../components/Toast";
import type { AppAdapter } from "../../lib/tauri";
import type { VocabularyEntry, VocabularySuggestion } from "../../lib/types";
import { normalizeError } from "../../lib/errors";
import { useAsyncAction } from "../../lib/useAsyncAction";
import { useI18n } from "../../lib/i18n";

const parseAliases = (value: string) => value.split(",").map((alias) => alias.trim()).filter(Boolean);
const countKey = (count: number) => count === 1 ? "vocab.count.one" as const
  : count % 10 >= 2 && count % 10 <= 4 && (count % 100 < 12 || count % 100 > 14) ? "vocab.count.few" as const : "vocab.count.many" as const;

export function VocabularyPage({
  adapter,
  onToast,
}: {
  adapter: AppAdapter;
  onToast: (message: string, kind: ToastKind) => void;
}) {
  const { t } = useI18n();
  const [items, setItems] = useState<VocabularyEntry[]>([]);
  const [suggestions, setSuggestions] = useState<VocabularySuggestion[]>([]);
  const [term, setTerm] = useState("");
  const [aliases, setAliases] = useState("");
  const [showAliases, setShowAliases] = useState(false);
  const [search, setSearch] = useState("");
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState("");
  const { busy, pendingKey, run } = useAsyncAction(onToast);
  const load = useCallback(async () => {
    setLoading(true);
    setLoadError("");
    try {
      setItems([...(await adapter.listVocabulary())]);
    } catch (error) {
      setLoadError(normalizeError(error));
      return;
    } finally {
      setLoading(false);
    }
    // Suggestions are a bonus: a failure here just leaves the section hidden.
    try { setSuggestions([...(await adapter.vocabularySuggestions())]); } catch { setSuggestions([]); }
  }, [adapter]);
  useEffect(() => { void load(); }, [load]);
  const filtered = useMemo(() => {
    const query = search.trim().toLocaleLowerCase();
    return items.filter((item) => [item.term, ...item.aliases].join(" ").toLocaleLowerCase().includes(query));
  }, [items, search]);
  // The backend merges into an existing term, so the entry replaces by id.
  const store = (item: VocabularyEntry) => setItems((current) =>
    current.some(({ id }) => id === item.id) ? current.map((entry) => entry.id === item.id ? item : entry) : [...current, item]);

  const add = async () => {
    if (!term.trim() || busy) return;
    await run("add", async () => {
      store(await adapter.addVocabulary(term.trim(), parseAliases(aliases)));
      setTerm("");
      setAliases("");
    }, "vocab.error.add");
  };
  const remove = async (item: VocabularyEntry) => {
    await run(`delete-${item.id}`, async () => {
      if (await adapter.deleteVocabulary(item.id)) {
        setItems((current) => current.filter(({ id }) => id !== item.id));
        setSuggestions((current) => current.filter(({ termId }) => termId !== item.id));
      }
    }, "vocab.error.remove");
  };
  const accept = async (suggestion: VocabularySuggestion) => {
    await run(`suggest-${suggestion.termId}-${suggestion.heard}`, async () => {
      store(await adapter.addVocabulary(suggestion.term, [suggestion.heard]));
      setSuggestions((current) => current.filter((item) => item !== suggestion));
    }, "vocab.error.add");
  };
  const submitOnEnter = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "Enter") { event.preventDefault(); void add(); }
  };

  return (
    <section className="page vocabulary-page">
      <header className="page-header"><div><h1>{t("vocab.title")}</h1><p>{t("vocab.subtitle")}</p><p className="vocabulary-intro">{t("vocab.intro")}</p></div></header>
      <div className="vocabulary-add">
        <label><span>{t("vocab.term.label")}</span><input value={term} onChange={(event) => setTerm(event.target.value)} onKeyDown={submitOnEnter} placeholder={t("vocab.term.placeholder")} /></label>
        <button className="primary-button" disabled={!term.trim() || busy} onClick={() => void add()}><Plus size={16} />{pendingKey === "add" ? t("vocab.adding") : t("vocab.add")}</button>
        {showAliases && <label className="vocabulary-aliases"><span>{t("vocab.aliases.label")}</span><input value={aliases} onChange={(event) => setAliases(event.target.value)} onKeyDown={submitOnEnter} placeholder={t("vocab.aliases.placeholder")} /></label>}
      </div>
      <button className="text-button vocabulary-toggle" aria-expanded={showAliases} onClick={() => setShowAliases((open) => !open)}>{t("vocab.aliases.toggle")}<ChevronDown size={14} /></button>
      <div className="section-heading"><div><h2>{t("vocab.saved.title")}</h2><p>{t(countKey(items.length), { count: items.length })}</p></div><label className="search-field search-field--small"><Search size={16} /><span className="sr-only">{t("vocab.search.label")}</span><input type="search" value={search} onChange={(event) => setSearch(event.target.value)} placeholder={t("vocab.search.placeholder")} /></label></div>
      <div className="vocabulary-list">
        {loading ? <div className="panel-loading" role="status">{t("vocab.loading")}</div> :
          loadError ? <EmptyState title={t("vocab.loadError.title")} description={loadError} action={<button className="primary-button" onClick={() => void load()}>{t("common.retryAction")}</button>} /> : <>
            {filtered.map((item) => <div className="vocabulary-row" key={item.id}>
              <div className="vocabulary-row__text"><strong>{item.term}</strong>{item.aliases.length > 0 && <small>{t("vocab.aliases.list", { aliases: item.aliases.join(", ") })}</small>}</div>
              <button disabled={busy} className="icon-button" aria-label={t("vocab.removeAria", { word: item.term })} title={t("vocab.removeAria", { word: item.term })} onClick={() => void remove(item)}><Trash2 size={16} /></button>
            </div>)}
            {!filtered.length && <EmptyState title={t("vocab.empty.title")} description={t("vocab.empty.description")} />}
          </>}
      </div>
      {!loading && !loadError && suggestions.length > 0 && <>
        <div className="section-heading"><div><h2>{t("vocab.suggestions.title")}</h2><p>{t("vocab.suggestions.subtitle")}</p></div></div>
        <div className="vocabulary-list">
          {suggestions.map((item) => <div className="vocabulary-row" key={`${item.termId}-${item.heard}`}>
            <div className="vocabulary-row__text">
              <div className="vocabulary-suggestion__pair"><span>{item.heard}</span><span aria-hidden="true">→</span><strong>{item.term}</strong><em>{t("vocab.suggestions.count", { count: item.count })}</em></div>
              {item.example && <small title={item.example}>{item.example}</small>}
            </div>
            <button disabled={busy} className="secondary-button" aria-label={t("vocab.suggestions.addAria", { heard: item.heard, term: item.term })} onClick={() => void accept(item)}>{t("vocab.suggestions.add")}</button>
          </div>)}
        </div>
      </>}
    </section>
  );
}
