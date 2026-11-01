import { StrictMode } from "react";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { I18nProvider } from "../lib/i18n";
import { StartupScreen } from "./StartupScreen";

function scene(loading = false) {
  return <StrictMode><I18nProvider><StartupScreen loading={loading}><p>Ready content</p></StartupScreen></I18nProvider></StrictMode>;
}
describe("StartupScreen", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.stubGlobal("matchMedia", vi.fn(() => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() })));
  });
  afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); });
  it("reveals the working app after one short entrance, including StrictMode", () => {
    const view = render(scene());
    expect(screen.queryByText("Ready content")).not.toBeInTheDocument();
    act(() => vi.advanceTimersByTime(820));
    expect(screen.getByText("Ready content")).toBeVisible();
    view.rerender(scene());
    expect(screen.getByText("Ready content")).toBeVisible();
  });
  it("waits for actual initialization after the brand entrance finishes", () => {
    const view = render(scene(true));
    act(() => vi.advanceTimersByTime(820));
    expect(screen.queryByText("Ready content")).not.toBeInTheDocument();
    expect(screen.getByRole("status")).toBeVisible();
    view.rerender(scene(false));
    expect(screen.getByText("Ready content")).toBeVisible();
  });
  it("lets a ready user skip the entrance", () => {
    render(scene());
    fireEvent.click(screen.getByRole("button"));
    expect(screen.getByText("Ready content")).toBeVisible();
  });
  it("skips decorative motion for reduced-motion users", () => {
    vi.stubGlobal("matchMedia", vi.fn(() => ({ matches: true, addEventListener: vi.fn(), removeEventListener: vi.fn() })));
    render(scene());
    expect(screen.getByText("Ready content")).toBeVisible();
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });
  it("removes its pending timer when unmounted", () => {
    const view = render(scene());
    view.unmount();
    expect(vi.getTimerCount()).toBe(0);
  });
});
