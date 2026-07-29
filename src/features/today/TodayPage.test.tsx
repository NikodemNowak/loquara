import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";

import { adapterStub, settings } from "../../test/fixtures";
import type { DictationState } from "../../lib/types";
import { TodayPage } from "./TodayPage";

function renderState(state: DictationState, overrides = {}) {
  const onToast = vi.fn();
  const adapter = adapterStub(overrides);
  const view = render(<TodayPage
    adapter={adapter}
    snapshot={{ dictation: state, settings }}
    recordings={[]}
    onSnapshot={() => undefined}
    onHistory={() => undefined}
    onToast={onToast}
  />);
  return { adapter, onToast, ...view };
}

describe("CTA strony Dzisiaj", () => {
  test("aktualizuje oba zestawy klawiszy po zmianie skonfigurowanego skrótu", () => {
    const props = {
      adapter: adapterStub(),
      recordings: [],
      onSnapshot: () => undefined,
      onHistory: () => undefined,
      onToast: () => undefined,
    };
    const { rerender } = render(<TodayPage
      {...props}
      snapshot={{ dictation: { status: "idle" }, settings }}
    />);
    expect(screen.getAllByLabelText("Skrót klawiszowy: Ctrl + Space")).toHaveLength(2);

    rerender(<TodayPage
      {...props}
      snapshot={{
        dictation: { status: "idle" },
        settings: { ...settings, shortcut: "Ctrl+Shift+M" },
      }}
    />);

    const shortcuts = screen.getAllByLabelText("Skrót klawiszowy: Ctrl + Shift + M");
    expect(shortcuts).toHaveLength(2);
    for (const shortcut of shortcuts) {
      expect(Array.from(shortcut.querySelectorAll("kbd"), (key) => key.textContent)).toEqual([
        "Ctrl",
        "Shift",
        "M",
      ]);
    }
    expect(screen.queryByText("Spacja")).not.toBeInTheDocument();
  });

  test.each([
    [{ status: "processing", recordingId: "a", audioPath: "a.wav" }, "Przepisuję…"],
    [{ status: "pasting", recordingId: "a", audioPath: "a.wav", transcript: "tekst" }, "Wklejam…"],
  ] satisfies Array<[DictationState, string]>)("blokuje akcję w stanie %s", (state, label) => {
    renderState(state);
    expect(screen.getByRole("button", { name: new RegExp(label) })).toBeDisabled();
  });

  test("uruchamia, zatrzymuje i ponawia właściwe komendy", async () => {
    const startRecording = vi.fn(async () => ({ dictation: { status: "idle" as const }, settings }));
    const first = renderState({ status: "idle" }, { startRecording });
    await userEvent.click(screen.getByRole("button", { name: /Zacznij mówić/ }));
    expect(startRecording).toHaveBeenCalledOnce();
    first.unmount();

    const stopRecording = vi.fn(async () => ({ dictation: { status: "idle" as const }, settings }));
    const second = renderState({ status: "recording", recordingId: "r", audioPath: "r.wav" }, { stopRecording });
    await userEvent.click(screen.getByRole("button", { name: /Zatrzymaj nagrywanie/ }));
    expect(stopRecording).toHaveBeenCalledOnce();
    second.unmount();

    const retryTranscription = vi.fn(async () => ({ dictation: { status: "idle" as const }, settings }));
    renderState({ status: "failed", recovery: { recordingId: "failed", audioPath: "f.wav" }, error: "Błąd" }, { retryTranscription });
    await userEvent.click(screen.getByRole("button", { name: /Ponów/ }));
    expect(retryTranscription).toHaveBeenCalledWith("failed");
  });

  test("pokazuje błąd komendy i przywraca przycisk", async () => {
    const startRecording = vi.fn(async () => { throw new Error("Mikrofon zajęty"); });
    const { onToast } = renderState({ status: "idle" }, { startRecording });
    await userEvent.click(screen.getByRole("button", { name: /Zacznij mówić/ }));
    await waitFor(() => expect(onToast).toHaveBeenCalledWith(expect.stringContaining("Mikrofon zajęty"), "error"));
    expect(screen.getByRole("button", { name: /Zacznij mówić/ })).toBeEnabled();
  });
});
