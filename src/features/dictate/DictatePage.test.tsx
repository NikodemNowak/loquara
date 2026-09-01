import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";

import { renderWithI18n } from "../../test/renderWithI18n";
import { adapterStub, recordings, settings } from "../../test/fixtures";
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

  test("bez modelu nie twierdzi, że jest gotowa", async () => {
    // Zgłoszone z czystej instalacji: aplikacja pokazywała "Gotowy", mimo że
    // żaden model nie był pobrany, więc skrót mógł się tylko wysypać.
    renderWithI18n(<DictatePage
      adapter={adapterStub()}
      snapshot={{ dictation: { status: "idle" }, settings, model: { key: "parakeet", display: "Parakeet TDT 0.6B v3", provider: "NVIDIA", installed: false, totalBytes: 670_478_772 }, modelLoading: false }}
      recordings={[]}
      modelReady={false}
      onSnapshot={() => undefined}
      onHistory={() => undefined}
      onSettings={() => undefined}
      onToast={() => undefined}
    />);

    expect(screen.getByRole("heading", { name: "Potrzebny jest model" })).toBeVisible();
    expect(screen.getByText("Parakeet TDT 0.6B v3 · 670 MB · NVIDIA")).toBeVisible();
    expect(screen.queryByRole("heading", { name: "Gotowy" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Zacznij nagrywać" })).not.toBeInTheDocument();
  });

  test("pobieranie zaczęte gdzie indziej widać po powrocie na ekran", async () => {
    // Stan pobierania należy do aplikacji, nie do ekranu. Trzymany w
    // komponencie znikał po wyjściu, a przycisk wracał gotowy do klikania —
    // i drugie pobieranie pisało po plikach pierwszego.
    const model = { key: "parakeet", display: "Parakeet TDT 0.6B v3", provider: "NVIDIA", installed: false, totalBytes: 670_478_772 };
    const downloadModel = vi.fn(async () => undefined);
    renderWithI18n(<DictatePage
      adapter={adapterStub({ downloadModel })}
      snapshot={{
        dictation: { status: "idle" },
        settings,
        model,
        download: { model: "parakeet", downloadedBytes: 335_000_000, totalBytes: 670_478_772 },
        modelLoading: false,
      }}
      recordings={[]}
      modelReady={false}
      onSnapshot={() => undefined}
      onHistory={() => undefined}
      onSettings={() => undefined}
      onToast={() => undefined}
    />);

    expect(await screen.findByText("335 MB z 670 MB")).toBeVisible();
    expect(screen.queryByRole("button", { name: "Pobierz model" })).toBeNull();
    expect(downloadModel).not.toHaveBeenCalled();
  });

  test("bez modelu pobiera go na miejscu, bez wysyłania gdzie indziej", async () => {
    // Przycisk prowadził do Ustawień, gdzie sekcja modeli była poniżej
    // zgięcia — klikało się i pozornie nic się nie działo.
    const startRecording = vi.fn();
    const downloadModel = vi.fn(async () => undefined);
    const onSettings = vi.fn();
    renderWithI18n(<DictatePage
      adapter={adapterStub({ startRecording, downloadModel })}
      snapshot={{ dictation: { status: "idle" }, settings, model: { key: "parakeet", display: "Parakeet TDT 0.6B v3", provider: "NVIDIA", installed: false, totalBytes: 670_478_772 }, modelLoading: false }}
      recordings={[]}
      modelReady={false}
      onSnapshot={() => undefined}
      onHistory={() => undefined}
      onSettings={onSettings}
      onToast={() => undefined}
    />);

    await userEvent.click(screen.getByRole("button", { name: "Pobierz model" }));

    expect(downloadModel).toHaveBeenCalledWith("parakeet");
    expect(onSettings).not.toHaveBeenCalled();
    expect(startRecording).not.toHaveBeenCalled();

    // Katalog ma dziś jeden model, więc ekran tego nie ukrywa.
    expect(screen.getByText("Na razie jeden model. Kolejne wkrótce.")).toBeVisible();
  });

  test("kliknięcie ostatniego nagrania otwiera historię na tym nagraniu", async () => {
    const onHistory = vi.fn();
    renderWithI18n(<DictatePage
      adapter={adapterStub()}
      snapshot={{ dictation: { status: "idle" }, settings, modelLoading: false }}
      recordings={recordings}
      onSnapshot={() => undefined}
      onHistory={onHistory}
      onToast={() => undefined}
    />);

    await userEvent.click(screen.getByRole("button", { name: /Przygotuj proszę podsumowanie/ }));
    expect(onHistory).toHaveBeenCalledWith("complete-1");
  });

  test("cała historia otwiera listę bez wybranego nagrania", async () => {
    const onHistory = vi.fn();
    renderWithI18n(<DictatePage
      adapter={adapterStub()}
      snapshot={{ dictation: { status: "idle" }, settings, modelLoading: false }}
      recordings={recordings}
      onSnapshot={() => undefined}
      onHistory={onHistory}
      onToast={() => undefined}
    />);

    await userEvent.click(screen.getByRole("button", { name: "Cała historia" }));
    expect(onHistory).toHaveBeenCalledOnce();
    expect(onHistory.mock.calls[0]).toEqual([]);
  });
});
