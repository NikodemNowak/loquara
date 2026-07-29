import { X } from "./Icons";

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
  return (
    <div className="toast-region" role="region" aria-label="Powiadomienia">
      {items.map((item) => (
        <div className={`toast toast--${item.kind}`} role="status" key={item.id}>
          <span>{item.text}</span>
          <button className="icon-button" onClick={() => onDismiss(item.id)} aria-label="Zamknij powiadomienie">
            <X size={15} />
          </button>
        </div>
      ))}
    </div>
  );
}
