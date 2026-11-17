import { useEffect, useRef, useState } from "react";

import { CheckCircle, ChevronDown, ErrorCircle, Info, X } from "./Icons";
import { useI18n } from "../lib/i18n";

export type ToastKind = "error" | "success" | "info";

export interface ToastMessage {
  id: number;
  text: string;
  kind: ToastKind;
  /** Raw backend text, shown behind a "details" disclosure for debugging. */
  detail?: string;
}

/** Errors stay long enough to be read; confirmations get out of the way. */
const lifetime: Record<ToastKind, number> = { error: 8000, info: 5000, success: 3500 };

const icons = { error: ErrorCircle, success: CheckCircle, info: Info } as const;

export function NoticeIcon({ kind }: { kind: ToastKind }) {
  const Icon = icons[kind];
  return <Icon size={16} className={`notice-icon notice-icon--${kind}`} />;
}

export function NoticeDetails({ detail }: { detail?: string }) {
  const { t } = useI18n();
  if (!detail) return null;
  return (
    <details className="notice-details">
      <summary>{t("notice.details")}<ChevronDown size={12} className="notice-details__chevron" /></summary>
      <code>{detail}</code>
    </details>
  );
}

/** An inline error inside a page: the same icon-and-words shape as a toast. */
export function ErrorNote({ message, detail, live = false }: { message: string; detail?: string; live?: boolean }) {
  return (
    <div className="error-note" role={live ? "alert" : undefined}>
      <NoticeIcon kind="error" />
      <div className="error-note__body">
        <p>{message}</p>
        <NoticeDetails detail={detail} />
      </div>
    </div>
  );
}

function ToastItem({ item, onDismiss }: { item: ToastMessage; onDismiss: (id: number) => void }) {
  const { t } = useI18n();
  // Reading or reaching for the toast holds it; leaving lets it time out again.
  const [held, setHeld] = useState(false);
  const dismiss = useRef(onDismiss);
  dismiss.current = onDismiss;
  useEffect(() => {
    if (held) return;
    const timer = window.setTimeout(() => dismiss.current(item.id), lifetime[item.kind]);
    return () => window.clearTimeout(timer);
  }, [held, item.id, item.kind]);

  return (
    <li
      className={`toast toast--${item.kind}`}
      role={item.kind === "error" ? "alert" : "status"}
      onMouseEnter={() => setHeld(true)}
      onMouseLeave={() => setHeld(false)}
      onFocus={() => setHeld(true)}
      onBlur={(event) => { if (!event.currentTarget.contains(event.relatedTarget)) setHeld(false); }}
    >
      <NoticeIcon kind={item.kind} />
      <div className="toast__body">
        <p className="toast__text">{item.text}</p>
        <NoticeDetails detail={item.detail} />
      </div>
      <button className="icon-button notice-dismiss" onClick={() => onDismiss(item.id)} aria-label={t("toast.close")} title={t("toast.close")}>
        <X size={14} />
      </button>
    </li>
  );
}

export function ToastRegion({
  items,
  onDismiss,
}: {
  items: ToastMessage[];
  onDismiss: (id: number) => void;
}) {
  const { t } = useI18n();
  return (
    <section className="toast-region" aria-label={t("toast.region")}>
      <ol>
        {items.map((item) => <ToastItem key={item.id} item={item} onDismiss={onDismiss} />)}
      </ol>
    </section>
  );
}
