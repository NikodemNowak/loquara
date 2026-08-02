import { useCallback, useEffect, useMemo, useState } from "react";

import { Plus, Search, Trash2 } from "../../components/Icons";
import { EmptyState } from "../../components/EmptyState";
import type { ToastKind } from "../../components/Toast";
import type { AppAdapter } from "../../lib/tauri";
import type { VocabularyEntry } from "../../lib/types";
import { normalizeError } from "../../lib/errors";
import { useAsyncAction } from "../../lib/useAsyncAction";
import { useI18n } from "../../lib/i18n";

export function VocabularyPage({
  adapter,
  onToast,
}: {
  adapter: AppAdapter;
  onToast: (message: string, kind: ToastKind) => void;
}) {
  const { t } = useI18n();
  const [items, setItems] = useState<VocabularyEntry[]>([]);
  const [heard, setHeard] = useState("");
  const [replacement, setReplacement] = useState("");
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
    } finally {
      setLoading(false);
    }
  }, [adapter]);
  useEffect(() => { void load(); }, [load]);
  const filtered = useMemo(() => items.filter((item) => `${item.heard} ${item.replacement}`.toLocaleLowerCase("pl").includes(search.toLocaleLowerCase("pl"))), [items, search]);

  const add = async () => {
    if (!heard.trim() || !replacement.trim()) return;
    await run("add", async () => {
      const item = await adapter.addVocabulary(heard.trim(), replacement.trim());
      setItems((current) => [...current.filter(({ id }) => id !== item.id), item]);
      setHeard("");
      setReplacement("");
    }, "vocab.error.add");
  };
  const remove = async (item: VocabularyEntry) => {
    await run(`delete-${item.id}`, async () => {
      if (await adapter.deleteVocabulary(item.id)) {
        setItems((current) => current.filter(({ id }) => id !== item.id));
      }
    }, "vocab.error.remove");
  };

  return (
    <section className="page vocabulary-page">
      <header className="page-header"><div><p className="eyebrow">{t("vocab.eyebrow")}</p><h1>{t("vocab.title")}</h1><p>{t("vocab.subtitle")}</p></div></header>
      <div className="vocabulary-add">
        <label><span>{t("vocab.heard.label")}</span><input value={heard} onChange={(event) => setHeard(event.target.value)} placeholder={t("vocab.heard.placeholder")} /></label>
        <span className="replace-arrow" aria-hidden="true">→</span>
        <label><span>{t("vocab.replacement.label")}</span><input value={replacement} onChange={(event) => setReplacement(event.target.value)} placeholder={t("vocab.replacement.placeholder")} /></label>
        <button className="primary-button" disabled={!heard.trim() || !replacement.trim() || busy} onClick={() => void add()}><Plus size={16} />{pendingKey === "add" ? t("vocab.adding") : t("vocab.add")}</button>
      </div>
      <div className="section-heading"><div><h2>{t("vocab.saved.title")}</h2><p>{items.length === 1 ? t("vocab.count.one", { count: items.length }) : t("vocab.count.many", { count: items.length })}</p></div><label className="search-field search-field--small"><Search size={16} /><span className="sr-only">{t("vocab.search.label")}</span><input type="search" value={search} onChange={(event) => setSearch(event.target.value)} placeholder={t("vocab.search.placeholder")} /></label></div>
      <div className="vocabulary-list">
        {loading ? <div className="panel-loading" role="status">{t("vocab.loading")}</div> :
          loadError ? <EmptyState title={t("vocab.loadError.title")} description={loadError} action={<button className="primary-button" onClick={() => void load()}>{t("common.retryAction")}</button>} /> : <>
            {filtered.map((item) => <div className="vocabulary-row" key={item.id}><span className="heard-word">{item.heard}</span><span>→</span><strong>{item.replacement}</strong><button disabled={busy} className="icon-button" aria-label={t("vocab.removeAria", { word: item.heard })} onClick={() => void remove(item)}><Trash2 size={16} /></button></div>)}
            {!filtered.length && <EmptyState title={t("vocab.empty.title")} description={t("vocab.empty.description")} />}
          </>}
      </div>
      <aside className="example-note"><strong>{t("vocab.how.title")}</strong><p>{t("vocab.how.body")}</p></aside>
    </section>
  );
}
