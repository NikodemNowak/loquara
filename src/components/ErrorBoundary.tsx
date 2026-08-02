import { Component, type ReactNode } from "react";

import { translate } from "../lib/i18n/lang";

export class ErrorBoundary extends Component<{ children: ReactNode }, { message: string }> {
  override state = { message: "" };

  static getDerivedStateFromError(error: unknown) {
    return { message: error instanceof Error ? error.message : String(error) };
  }

  override componentDidCatch(error: unknown) {
    console.error(error);
  }

  override render() {
    if (this.state.message) {
      return (
        <main className="error-boundary">
          <strong>{translate("errors.boundary.title")}</strong>
          <p>{this.state.message}</p>
          <button className="primary-button" onClick={() => window.location.reload()}>
            {translate("errors.boundary.reload")}
          </button>
        </main>
      );
    }
    return this.props.children;
  }
}
