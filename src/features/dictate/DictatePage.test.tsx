import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";

import { renderWithI18n } from "../../test/renderWithI18n";
import { adapterStub, settings } from "../../test/fixtures";
import type { DictationState } from "../../lib/types";
import { DictatePage } from "./DictatePage";

function renderState(state: DictationState, overrides = {}) {
  const onToast = vi.fn();
  const adapter = adapterStub(overrides);
  const view = renderWithI18n(<DictatePage
    adapter={adapter}
    snapshot={{ dictation: state, settings, modelLoading: false }}
    recordings={[]}
    onSnapshot={() => undefined}
    onHistory={() => undefined}
    onToast={onToast}
  />);
  return { adapter, onToast, ...view };
}

describe("ekran Dyktuj", () => {
  test("pokazuje aktualny skrót i odświeża go po zmianie ustawienia", () => {
    const props = {
      adapter: adapterStub(),
      recordings: [],
      onSnapshot: () => undefined,
      onHistory: () => undefined,
      onToast: () => undefined,
    };
    const { rerender } = renderWithI18n(<DictatePage
      {...props}
      snapshot={{ dictation: { status: "idle" }, settings, modelLoading: false }}
    />);
    expect(screen.getByLabelText("Skrót klawiszowy: Ctrl + Space")).toBeVisible();

    rerender(<DictatePage
      {...props}
      snapshot={{
        dictation: { status: "idle" },
        settings: { ...settings, shortcut: "Ctrl+Shift+M" },
        modelLoading: false,
      }}
    />);

    const shortcut = screen.getByLabelText("Skrót klawiszowy: Ctrl + Shift + M");
    expect(Array.from(shortcut.querySelectorAll("kbd"), (key) => key.textContent)).toEqual([
      "Ctrl",
      "Shift",
      "M",
    ]);
  });

  test.each([
    [{ status: "processing", recordingId: "a", audioPath: "a.wav" }, "Przetwarzam"],
    [{ status: "pasting", recordingId: "a", audioPath: "a.wav", transcript: "tekst" }, "Wklejam"],
  ] satisfies Array<[DictationState, string]>)(
    "w stanie %s pokazuje odczyt bez akcji do kliknięcia",
    (state, label) => {
      renderState(state);
      expect(screen.getByRole("heading", { name: label })).toBeVisible();
      expect(screen.queryByRole("button", { name: /Zacznij nagrywać|Zatrzymaj|Spróbuj ponownie/ }))
        .not.toBeInTheDocument();
    },
  );

  test("uruchamia, zatrzymuje i ponawia właściwe komendy", async () => {
    const startRecording = vi.fn(async () => ({ dictation: { status: "idle" as const }, settings, modelLoading: false }));
    const first = renderState({ status: "idle" }, { startRecording });
    await userEvent.click(screen.getByRole("button", { name: "Zacznij nagrywać" }));
    expect(startRecording).toHaveBeenCalledOnce();
    first.unmount();

    const stopRecording = vi.fn(async () => ({ dictation: { status: "idle" as const }, settings, modelLoading: false }));
    const second = renderState({ status: "recording", recordingId: "r", audioPath: "r.wav" }, { stopRecording });
    await userEvent.click(screen.getByRole("button", { name: "Zatrzymaj" }));
    expect(stopRecording).toHaveBeenCalledOnce();
    second.unmount();

    const retryTranscription = vi.fn(async () => ({ dictation: { status: "idle" as const }, settings, modelLoading: false }));
    renderState({ status: "failed", recovery: { recordingId: "failed", audioPath: "f.wav" }, error: "Błąd" }, { retryTranscription });
    await userEvent.click(screen.getByRole("button", { name: "Spróbuj ponownie" }));
    expect(retryTranscription).toHaveBeenCalledWith("failed");
  });

  test("pokazuje błąd komendy i przywraca przycisk", async () => {
    const startRecording = vi.fn(async () => { throw new Error("Mikrofon zajęty"); });
    const { onToast } = renderState({ status: "idle" }, { startRecording });
    await userEvent.click(screen.getByRole("button", { name: "Zacznij nagrywać" }));
    await waitFor(() => expect(onToast).toHaveBeenCalledWith(expect.stringContaining("Mikrofon zajęty"), "error"));
    expect(screen.getByRole("button", { name: "Zacznij nagrywać" })).toBeEnabled();
  });

  test("odlicza czas nagrania od momentu jego rozpoczęcia", () => {
    const startedAt = Date.now() - 65_000;
    renderWithI18n(<DictatePage
      adapter={adapterStub()}
      snapshot={{
        dictation: { status: "recording", recordingId: "r", audioPath: "r.wav" },
        settings,
        modelLoading: false,
        recordingStartedAt: startedAt,
      }}
      recordings={[]}
      onSnapshot={() => undefined}
      onHistory={() => undefined}
      onToast={() => undefined}
    />);

    expect(screen.getByLabelText("Czas nagrania")).toHaveTextContent("01:05");
  });
});
