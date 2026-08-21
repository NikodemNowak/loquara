import { useState } from "react";

import { Check, Copy } from "../../components/Icons";
import type { EngineStatus } from "../../lib/types";
import { useI18n, type TranslationKey } from "../../lib/i18n";

const PYTORCH_URL = "https://pytorch.org/get-started/locally/";
const PYTHON_URL = "https://www.python.org/downloads/windows/";

interface Step {
  done: boolean;
  title: TranslationKey;
  detail: TranslationKey;
  /** A command the user can paste, when there is one that would fix this. */
  command?: string;
  link?: { href: string; label: TranslationKey };
}

function CommandLine({ command }: { command: string }) {
  const { t } = useI18n();
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    if (!navigator.clipboard?.writeText) return;
    await navigator.clipboard.writeText(command);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1600);
  };

  return (
    <div className="command">
      <code>{command}</code>
      <button
        type="button"
        className="command__copy"
        onClick={() => void copy()}
        aria-label={t("setup.copyCommand")}
        title={t("setup.copyCommand")}
      >
        {copied ? <Check size={13} /> : <Copy size={13} />}
      </button>
    </div>
  );
}

/**
 * What is still missing before Loquara can transcribe, and how to fix it.
 *
 * Loquara ships without the Python engine, so a machine that has only run the
 * installer is missing all of this. Each step names one thing and gives the
 * exact command for it, pointing at the requirements file that shipped with
 * the app rather than at a repository the user may not have.
 */
export function EngineSetup({
  status,
  modelReady,
  onRecheck,
  checking,
}: {
  status: EngineStatus;
  modelReady: boolean;
  onRecheck: () => void;
  checking: boolean;
}) {
  const { t } = useI18n();

  const install = status.requirementsPath
    ? `${status.pythonCommand} -m pip install -r "${status.requirementsPath}"`
    : undefined;

  const steps: Step[] = [
    {
      done: status.python,
      title: "setup.python.title",
      detail: status.python ? "setup.python.found" : "setup.python.missing",
      link: status.python ? undefined : { href: PYTHON_URL, label: "setup.python.download" },
    },
    {
      done: status.dependencies,
      title: "setup.deps.title",
      detail: status.dependencies ? "setup.deps.done" : "setup.deps.missing",
      command: status.python && !status.dependencies ? install : undefined,
    },
    {
      done: status.torch,
      title: "setup.torch.title",
      detail: status.torch ? "setup.torch.done" : "setup.torch.missing",
      link: status.torch ? undefined : { href: PYTORCH_URL, label: "setup.torch.instructions" },
    },
    {
      done: modelReady,
      title: "setup.model.title",
      detail: modelReady ? "setup.model.done" : "setup.model.missing",
    },
  ];

  const remaining = steps.filter((step) => !step.done).length;
  if (remaining === 0) return null;

  return (
    <section className="setup" aria-label={t("setup.title")}>
      <div className="setup__head">
        <div>
          <h2>{t("setup.title")}</h2>
          <p>{t("setup.intro")}</p>
        </div>
        <button className="secondary-button" onClick={onRecheck} disabled={checking}>
          {checking ? t("setup.checking") : t("setup.recheck")}
        </button>
      </div>

      <ol className="setup__steps">
        {steps.map((step) => (
          <li key={step.title} className={step.done ? "setup__step setup__step--done" : "setup__step"}>
            <span className="setup__mark" aria-hidden="true">
              {step.done ? <Check size={12} /> : null}
            </span>
            <div className="setup__body">
              <strong>{t(step.title)}</strong>
              <small>{t(step.detail)}</small>
              {step.command && <CommandLine command={step.command} />}
              {step.link && (
                <a className="gate__link" href={step.link.href} target="_blank" rel="noreferrer">
                  {t(step.link.label)}
                </a>
              )}
            </div>
          </li>
        ))}
      </ol>
    </section>
  );
}
