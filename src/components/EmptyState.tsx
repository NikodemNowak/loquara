import type { ReactNode } from "react";

export function EmptyState({
  title,
  description,
  action,
  icon,
}: {
  title: string;
  description: string;
  action?: ReactNode;
  icon?: ReactNode;
}) {
  return (
    <div className="empty-state">
      {icon ? <span className="empty-state__icon" aria-hidden="true">{icon}</span> : null}
      <strong>{title}</strong>
      <p>{description}</p>
      {action}
    </div>
  );
}
