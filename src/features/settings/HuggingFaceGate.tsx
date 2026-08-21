import { useState } from "react";

import { Check, Cpu } from "../../components/Icons";
import type { AppAdapter } from "../../lib/tauri";
import type { HfAccount, ModelDescriptor } from "../../lib/types";
import { normalizeError } from "../../lib/errors";
import { useI18n } from "../../lib/i18n";

const TOKEN_PAGE = "https://huggingface.co/settings/tokens";

/**
 * Explains, in place, why a gated model will not download.
 *
 * A gated repository needs two separate things: a Hugging Face account that
 * has accepted the model's licence, and a token proving who is asking. A
 * token alone is not enough, which is why the licence step is spelled out
 * rather than left implied by an authentication error.
 */
export function HuggingFaceGate({
  adapter,
  model,
  account,
  onAccount,
}: {
  adapter: AppAdapter;
  model: ModelDescriptor;
  account: HfAccount;
  onAccount: (account: HfAccount) => void;
}) {
  const { t } = useI18n();
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const connect = async () => {
    setBusy(true);
    setError("");
    try {
      onAccount(await adapter.connectHfAccount(token));
      setToken("");
    } catch (failure) {
      setError(normalizeError(failure));
    } finally {
      setBusy(false);
    }
  };

  const disconnect = async () => {
    setBusy(true);
    setError("");
    try {
      onAccount(await adapter.disconnectHfAccount());
    } catch (failure) {
      setError(normalizeError(failure));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="gate">
      <p className="gate__title">
        <Cpu size={15} />
        {t("gate.title")}
      </p>
      <p className="gate__body">{t("gate.body")}</p>

      <ol className="gate__steps">
        <li>
          {t("gate.step.account")}{" "}
          <a className="gate__link" href={`https://huggingface.co/${model.id}`} target="_blank" rel="noreferrer">
            {t("gate.openModel")}
          </a>
        </li>
        <li>{t("gate.step.licence")}</li>
        <li>
          {t("gate.step.token")}{" "}
          <a className="gate__link" href={TOKEN_PAGE} target="_blank" rel="noreferrer">
            {t("gate.openTokens")}
          </a>
        </li>
      </ol>

      {account.connected ? (
        <p className="gate__status gate__status--ok">
          <Check size={14} />
          {account.name
            ? t("gate.connected", { name: account.name })
            : t("gate.connectedAnonymous")}
          <button className="text-button" disabled={busy} onClick={() => void disconnect()}>
            {t("gate.disconnect")}
          </button>
        </p>
      ) : (
        <div className="gate__form">
          <label className="sr-only" htmlFor="hf-token">{t("gate.token.label")}</label>
          <input
            id="hf-token"
            type="password"
            autoComplete="off"
            spellCheck={false}
            value={token}
            placeholder={t("gate.token.placeholder")}
            onChange={(event) => setToken(event.target.value)}
            onKeyDown={(event) => { if (event.key === "Enter" && token.trim()) void connect(); }}
          />
          <button className="primary-button" disabled={busy || !token.trim()} onClick={() => void connect()}>
            {busy ? t("gate.connecting") : t("gate.connect")}
          </button>
        </div>
      )}

      {error && <p className="gate__status gate__status--error" role="alert">{error}</p>}
    </div>
  );
}
