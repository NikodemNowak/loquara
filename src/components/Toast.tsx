import { X } from "./Icons";
import { useI18n } from "../lib/i18n";

export type ToastKind = "error" | "success" | "info";

export interface ToastMessage {
  id: number;
  text: string;
  kind: ToastKind;
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
    <div className="toast-region" role="region" aria-label={t("toast.region")}>
      {items.map((item) => (
        <div className={`toast toast--${item.kind}`} role="status" key={item.id}>
          <span>{item.text}</span>
          <button className="icon-button" onClick={() => onDismiss(item.id)} aria-label={t("toast.close")}>
            <X size={15} />
          </button>
        </div>
      ))}
    </div>
  );
}
