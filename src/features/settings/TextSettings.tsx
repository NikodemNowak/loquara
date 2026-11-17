import { useState } from "react";

import { Trash2 } from "../../components/Icons";
import { Select } from "../../components/Select";
import { useI18n } from "../../lib/i18n";
import type { AppProfile, AppSettings, FillerRemoval } from "../../lib/types";

type Save = (patch: Partial<AppSettings>) => Promise<void> | void;

/** Fillers, spoken commands, snippets and per-app profiles. */
export function TextSettings({ settings, save }: { settings: AppSettings; save: Save }) {
  const { t } = useI18n();
  const snippets = settings.snippets ?? [];
  const profiles = settings.appProfiles ?? [];
  const [trigger, setTrigger] = useState("");
  const [snippetText, setSnippetText] = useState("");
  const [app, setApp] = useState("");

  const addSnippet = () => {
    const spoken = trigger.trim();
    if (!spoken || !snippetText) return;
    const rest = snippets.filter((item) => item.trigger.toLocaleLowerCase() !== spoken.toLocaleLowerCase());
    void save({ snippets: [...rest, { trigger: spoken, text: snippetText }] });
    setTrigger("");
    setSnippetText("");
  };
  const addProfile = () => {
    const name = app.trim().toLocaleLowerCase().replace(/\.exe$/, "");
    if (!name || profiles.some((item) => item.app === name)) return;
    void save({ appProfiles: [...profiles, { app: name, noTrailingPeriod: false, lowercaseStart: false, pressEnter: false }] });
    setApp("");
  };
  const updateProfile = (index: number, patch: Partial<AppProfile>) =>
    void save({ appProfiles: profiles.map((item, at) => at === index ? { ...item, ...patch } : item) });

  return <>
    <section className="settings-group settings-group--composed">
      <div className="group-heading"><h2>{t("settings.text.cleanup.title")}</h2><p>{t("settings.text.cleanup.help")}</p></div>
      <div className="settings-controls">
        <label className="setting-row">
          <span><strong>{t("settings.text.fillers.label")}</strong><small>{t("settings.text.fillers.description")}</small></span>
          <Select
            label={t("settings.text.fillers.label")}
            value={settings.fillerRemoval ?? "light"}
            onChange={(next) => void save({ fillerRemoval: next as FillerRemoval })}
            options={[
              { value: "off", label: t("settings.text.fillers.off") },
              { value: "light", label: t("settings.text.fillers.light") },
              { value: "strong", label: t("settings.text.fillers.strong") },
            ]}
          />
        </label>
        <label className="setting-row">
          <span><strong>{t("settings.text.commands.label")}</strong><small>{t("settings.text.commands.description")}</small></span>
          <input type="checkbox" aria-label={t("settings.text.commands.label")} checked={settings.voiceCommands ?? true}
            onChange={(event) => void save({ voiceCommands: event.target.checked })} />
        </label>
      </div>
    </section>

    <section className="settings-group settings-group--composed">
      <div className="group-heading"><h2>{t("settings.text.snippets.title")}</h2><p>{t("settings.text.snippets.help")}</p></div>
      <div className="settings-controls">
        {snippets.length ? <ul className="text-rules" aria-label={t("settings.text.snippets.title")}>
          {snippets.map((item) => <li key={item.trigger} className="text-rule">
            <span className="text-rule__main"><strong>„{item.trigger}”</strong><small>{item.text}</small></span>
            <button type="button" className="icon-button" aria-label={t("settings.text.remove", { name: item.trigger })}
              onClick={() => void save({ snippets: snippets.filter((other) => other !== item) })}><Trash2 size={14} /></button>
          </li>)}
        </ul> : <p className="settings-empty">{t("settings.text.snippets.empty")}</p>}
        <form className="text-rule-form" onSubmit={(event) => { event.preventDefault(); addSnippet(); }}>
          <input value={trigger} onChange={(event) => setTrigger(event.target.value)} maxLength={80}
            placeholder={t("settings.text.snippets.triggerPlaceholder")} aria-label={t("settings.text.snippets.trigger")} />
          <textarea value={snippetText} onChange={(event) => setSnippetText(event.target.value)} maxLength={5000} rows={2}
            placeholder={t("settings.text.snippets.textPlaceholder")} aria-label={t("settings.text.snippets.text")} />
          <button type="submit" className="text-button" disabled={!trigger.trim() || !snippetText}>{t("settings.text.add")}</button>
        </form>
      </div>
    </section>

    <section className="settings-group settings-group--composed">
      <div className="group-heading"><h2>{t("settings.text.profiles.title")}</h2><p>{t("settings.text.profiles.help")}</p></div>
      <div className="settings-controls">
        {profiles.length ? <ul className="text-rules" aria-label={t("settings.text.profiles.title")}>
          {profiles.map((item, index) => <li key={item.app} className="text-rule text-rule--profile">
            <span className="text-rule__main"><strong>{item.app}</strong></span>
            <span className="text-rule__options">
              <label><input type="checkbox" checked={item.noTrailingPeriod}
                onChange={(event) => updateProfile(index, { noTrailingPeriod: event.target.checked })} />{t("settings.text.profiles.noPeriod")}</label>
              <label><input type="checkbox" checked={item.lowercaseStart}
                onChange={(event) => updateProfile(index, { lowercaseStart: event.target.checked })} />{t("settings.text.profiles.lowercase")}</label>
              <label><input type="checkbox" checked={item.pressEnter}
                onChange={(event) => updateProfile(index, { pressEnter: event.target.checked })} />{t("settings.text.profiles.enter")}</label>
            </span>
            <button type="button" className="icon-button" aria-label={t("settings.text.remove", { name: item.app })}
              onClick={() => void save({ appProfiles: profiles.filter((other) => other !== item) })}><Trash2 size={14} /></button>
          </li>)}
        </ul> : <p className="settings-empty">{t("settings.text.profiles.empty")}</p>}
        <form className="text-rule-form text-rule-form--inline" onSubmit={(event) => { event.preventDefault(); addProfile(); }}>
          <input value={app} onChange={(event) => setApp(event.target.value)} maxLength={80}
            placeholder={t("settings.text.profiles.appPlaceholder")} aria-label={t("settings.text.profiles.app")} />
          <button type="submit" className="text-button" disabled={!app.trim()}>{t("settings.text.add")}</button>
        </form>
      </div>
    </section>
  </>;
}
