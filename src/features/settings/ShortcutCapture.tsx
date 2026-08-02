import { Fragment, useEffect, useRef, useState } from "react";

import { useI18n } from "../../lib/i18n";

const MODIFIER_KEYS = new Set(["Control", "Alt", "Shift", "Meta"]);
const shortcutPattern = /^(?:(?:Ctrl|Alt|Shift|Meta)\+)+(?:[A-Z0-9]|Space|Enter|F(?:[1-9]|1[0-2]))$/;

function comboFromEvent(event: KeyboardEvent): { combo: string | null; partial: string[] } {
  const partial: string[] = [];
  if (event.ctrlKey) partial.push("Ctrl");
  if (event.altKey) partial.push("Alt");
  if (event.shiftKey) partial.push("Shift");
  if (event.metaKey) partial.push("Meta");
  if (MODIFIER_KEYS.has(event.key)) return { combo: null, partial };
  let key: string | null = null;
  if (event.key === " " || event.key === "Spacebar") key = "Space";
  else if (event.key === "Enter") key = "Enter";
  else if (/^f(?:[1-9]|1[0-2])$/i.test(event.key)) key = event.key.toUpperCase();
  else if (/^[a-z0-9]$/i.test(event.key)) key = event.key.toUpperCase();
  if (!key || partial.length === 0) return { combo: null, partial };
  const combo = [...partial, key].join("+");
  return { combo: shortcutPattern.test(combo) ? combo : null, partial };
}

function KeyChips({ parts }: { parts: string[] }) {
  return (
    <span className="shortcut shortcut--inline" aria-hidden="true">
      {parts.map((part, index) => (
        <Fragment key={`${part}-${index}`}>
          {index > 0 && <b>+</b>}
          <kbd>{part}</kbd>
        </Fragment>
      ))}
    </span>
  );
}

export function ShortcutCapture({
  value,
  disabled,
  onCapture,
  onActiveChange,
}: {
  value: string;
  disabled?: boolean;
  onCapture: (shortcut: string) => void;
  onActiveChange?: (active: boolean) => void;
}) {
  const { t } = useI18n();
  const [capturing, setCapturing] = useState(false);
  const [partial, setPartial] = useState<string[]>([]);
  const callbacks = useRef({ onCapture, onActiveChange });
  callbacks.current = { onCapture, onActiveChange };

  useEffect(() => {
    if (!capturing) return;
    callbacks.current.onActiveChange?.(true);
    const finish = () => {
      setCapturing(false);
      setPartial([]);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      event.preventDefault();
      event.stopPropagation();
      if (event.key === "Escape") {
        finish();
        return;
      }
      const { combo, partial: nextPartial } = comboFromEvent(event);
      if (combo) {
        finish();
        callbacks.current.onCapture(combo);
        return;
      }
      setPartial(nextPartial);
    };
    const onBlur = () => finish();
    window.addEventListener("keydown", onKeyDown, true);
    window.addEventListener("blur", onBlur);
    return () => {
      window.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("blur", onBlur);
      callbacks.current.onActiveChange?.(false);
    };
  }, [capturing]);

  const parts = value.split("+").map((part) => part.trim()).filter(Boolean);
  return (
    <button
      type="button"
      className={capturing ? "shortcut-capture shortcut-capture--active" : "shortcut-capture"}
      disabled={disabled}
      aria-label={capturing ? t("settings.shortcut.press") : t("settings.shortcut.recordAria", { shortcut: parts.join(" + ") })}
      onClick={() => {
        setPartial([]);
        setCapturing(true);
      }}
    >
      {capturing
        ? partial.length
          ? <KeyChips parts={[...partial, "…"]} />
          : <span className="shortcut-capture__prompt">{t("settings.shortcut.press")}</span>
        : <KeyChips parts={parts} />}
    </button>
  );
}
