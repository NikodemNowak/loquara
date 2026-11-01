import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";

import { renderWithI18n } from "../../test/renderWithI18n";
import { adapterStub, recordings, settings } from "../../test/fixtures";
import type { AppSnapshot, DictationState } from "../../lib/types";
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
  test.each(["pl", "en"] as const)("opisuje przytrzymanie skrótu, ale przyciski nadal zaczynają i kończą kliknięciem (%s)", async (language) => {
    const holdSettings = { ...settings, activationMode: "push_to_talk" as const, shortcut: "Ctrl+Shift+M" };
    const idle: AppSnapshot = { settings: holdSettings, modelLoading: false, dictation: { status: "idle" } };
    const startRecording = vi.fn(async () => idle);
    const stopRecording = vi.fn(async () => idle);
    const props = { adapter: adapterStub({ startRecording, stopRecording }), recordings: [], onSnapshot: vi.fn(), onHistory: vi.fn(), onToast: vi.fn() };
    const view = renderWithI18n(<DictatePage {...props} snapshot={idle} />, { language });
    expect(screen.getByText(language === "pl" ? "Ustaw kursor w polu tekstowym. Przytrzymaj skrót i mów, puść, aby zakończyć." : "Place your cursor in a text field. Hold the shortcut and speak, release it to finish.")).toBeVisible();
    expect(screen.getByLabelText(language === "pl" ? "Skrót klawiszowy: Ctrl + Shift + M" : "Keyboard shortcut: Ctrl + Shift + M")).toBeVisible();
    await userEvent.click(screen.getByRole("button", { name: language === "pl" ? "Zacznij nagrywać" : "Start recording" }));
    expect(startRecording).toHaveBeenCalledOnce();
    expect(stopRecording).not.toHaveBeenCalled();
    view.rerender(<DictatePage {...props} snapshot={{ ...idle, dictation: { status: "recording", recordingId: "r", audioPath: "r.wav" } }} />);
    expect(screen.getByText(language === "pl" ? "Jeśli nagrywasz skrótem, puść klawisze. Możesz też kliknąć Zatrzymaj." : "If you started with the shortcut, release the keys. You can also click Stop.")).toBeVisible();
    await userEvent.click(screen.getByRole("button", { name: language === "pl" ? "Zatrzymaj" : "Stop" }));
    expect(stopRecording).toHaveBeenCalledOnce();
    view.rerender(<DictatePage {...props} snapshot={{ ...idle, settings }} />);
    expect(screen.getByText(language === "pl" ? "Ustaw kursor w polu tekstowym i użyj skrótu." : "Place your cursor in a text field and use the shortcut.")).toBeVisible();
  });

  test("stan gotowy wyjaśnia rozpoczęcie zamiast zatrzymania nagrania", () => {
    renderState({ status: "idle" });
    expect(screen.getAllByText("Ustaw kursor w polu tekstowym i użyj skrótu.")[0]).toBeVisible();
    expect(screen.queryByText("Zatrzymaj nagranie, aby przekazać gotowy tekst.")).not.toBeInTheDocument();
  });

  test.each(["pl", "en"] as const)("pusty ekran nagrania opisuje faktyczną fazę podglądu (%s)", (language) => {
    const props = { adapter: adapterStub(), recordings: [], onSnapshot: vi.fn(), onHistory: vi.fn(), onToast: vi.fn() };
    const phaseCopy = language === "pl" ? {
      after: "Tekst pojawi się po zatrzymaniu nagrania.", connecting: "Łączenie z podglądem na żywo…",
      listening: "Mów dalej. Czekamy na pierwsze słowa.", finalizing: "Kończymy transkrypcję…",
    } : {
      after: "Your text appears after you stop recording.", connecting: "Connecting to live preview…",
      listening: "Keep speaking. Waiting for the first words.", finalizing: "Finalizing the transcript…",
    };
    const cases = [
      [null, phaseCopy.after], ["connecting", phaseCopy.connecting], ["listening", phaseCopy.listening],
      ["draft", phaseCopy.listening], ["fallback", phaseCopy.after], ["finalizing", phaseCopy.finalizing],
    ] as const;
    for (const [phase, copy] of cases) {
      const view = renderWithI18n(<DictatePage {...props} snapshot={{
        settings: { ...settings, language }, modelLoading: false,
        dictation: { status: "recording", recordingId: "r", audioPath: "r.wav" },
        liveTranscript: phase ? { recordingId: "r", text: "", revision: 1, status: phase, clipboardReady: false } : null,
      }} />, { language });
      expect(screen.getByText(copy)).toBeVisible();
      expect(screen.queryByText(language === "pl" ? "Zacznij mówić. Tekst pojawi się tutaj." : "Start speaking. Your words appear here.")).not.toBeInTheDocument();
      view.unmount();
    }
  });

  test("bieżący dokument podąża za tekstem, ale zachowuje celowe przewinięcie w górę", () => {
    const props = { adapter: adapterStub(), recordings: [], onSnapshot: vi.fn(), onHistory: vi.fn(), onToast: vi.fn() };
    const take: AppSnapshot = { settings, modelLoading: false, dictation: { status: "recording", recordingId: "r", audioPath: "r.wav" }, liveTranscript: { recordingId: "r", revision: 1, status: "draft", text: "Pierwsze słowa", clipboardReady: true } };
    const view = renderWithI18n(<DictatePage {...props} snapshot={take} />);
    const canvas = screen.getByRole("region", { name: "Wersja robocza" });
    let height = 500;
    Object.defineProperties(canvas, { scrollHeight: { get: () => height }, clientHeight: { value: 180 }, scrollTop: { value: 0, writable: true } });
    view.rerender(<DictatePage {...props} snapshot={{ ...take, liveTranscript: { ...take.liveTranscript!, text: "Pierwsze słowa i dalsza część", revision: 2 } }} />);
    expect(canvas.scrollTop).toBe(320);
    canvas.scrollTop = 40;
    fireEvent.scroll(canvas);
    height = 700;
    view.rerender(<DictatePage {...props} snapshot={{ ...take, liveTranscript: { ...take.liveTranscript!, text: "Jeszcze dłuższy dokument", revision: 3 } }} />);
    expect(canvas.scrollTop).toBe(40);
    expect(canvas).toHaveTextContent("Jeszcze dłuższy dokument");
  });
  test("pokazuje i zastępuje tylko wersję roboczą bieżącego nagrania, usuwa ją przy anulowaniu", () => {
    const props = { adapter: adapterStub(), recordings, onSnapshot: vi.fn(), onHistory: vi.fn(), onToast: vi.fn() };
    const current: AppSnapshot = {
      settings, modelLoading: false,
      dictation: { status: "recording", recordingId: "current", audioPath: "current.wav" },
      liveTranscript: { recordingId: "old", text: "Nieaktualny tekst", revision: 1, status: "draft", clipboardReady: true },
    };
    const view = renderWithI18n(<DictatePage {...props} snapshot={current} />);
    const canvas = screen.getByRole("region", { name: "Wersja robocza" });
    expect(canvas).not.toHaveTextContent("Nieaktualny tekst");
    expect(canvas).not.toHaveTextContent("Przygotuj proszę");
    view.rerender(<DictatePage {...props} snapshot={{ ...current, liveTranscript: { ...current.liveTranscript!, recordingId: "current", text: "Dzień dobry", clipboardReady: false } }} />);
    expect(canvas).toHaveTextContent("Dzień dobry");
    expect(screen.queryByText("W schowku")).not.toBeInTheDocument();
    view.rerender(<DictatePage {...props} snapshot={{ ...current, liveTranscript: { ...current.liveTranscript!, recordingId: "current", text: "Dzień dobry Państwu.", revision: 2 } }} />);
    expect(canvas).toHaveTextContent("Dzień dobry Państwu.");
    expect(screen.getByText("W schowku")).toBeVisible();
    view.rerender(<DictatePage {...props} snapshot={{ ...current, dictation: { status: "cancelling", recordingId: "current", audioPath: "current.wav" }, liveTranscript: { ...current.liveTranscript!, recordingId: "current", text: "Dzień dobry Państwu." } }} />);
    expect(canvas).not.toHaveTextContent("Dzień dobry");
  });

  test("kopiuje dokładnie ostatni tekst i otwiera jego nagranie", async () => {
    const user = userEvent.setup();
    const writeText = vi.spyOn(navigator.clipboard, "writeText");
    const onHistory = vi.fn();
    renderWithI18n(<DictatePage adapter={adapterStub()} snapshot={{ dictation: { status: "idle" }, settings, modelLoading: false }} recordings={recordings} onSnapshot={vi.fn()} onHistory={onHistory} onToast={vi.fn()} />);
    expect(within(screen.getByRole("region", { name: "Ostatnia transkrypcja" })).getByText(recordings[2].text!)).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Kopiuj" }));
    expect(writeText).toHaveBeenCalledWith(recordings[2].text);
    await user.click(screen.getByRole("button", { name: /Przygotuj proszę podsumowanie/ }));
    expect(onHistory).toHaveBeenCalledWith("complete-1");
  });

  test("anuluje aktywne nagranie przez rzeczywistą komendę", async () => {
    const cancelRecording = vi.fn(async () => ({ dictation: { status: "idle" as const }, settings, modelLoading: false }));
    renderState({ status: "recording", recordingId: "r", audioPath: "r.wav" }, { cancelRecording });
    await userEvent.click(screen.getByRole("button", { name: "Anuluj nagranie" }));
    expect(cancelRecording).toHaveBeenCalledOnce();
  });

  test("ostatnie słowa ustępują bieżącemu nagraniu i wracają po anulowaniu", () => {
    const recent = [recordings[2], { ...recordings[2], id: "older", text: "Poprzednia wiadomość." }];
    const props = { adapter: adapterStub(), recordings: recent, onSnapshot: vi.fn(), onHistory: vi.fn(), onToast: vi.fn() };
    const idle: AppSnapshot = { settings, modelLoading: false, dictation: { status: "idle" } };
    const view = renderWithI18n(<DictatePage {...props} snapshot={idle} />);
    expect(screen.getByText("Poprzednia wiadomość.")).toBeVisible();
    expect(screen.getAllByRole("article")).toHaveLength(2);
    view.rerender(<DictatePage {...props} snapshot={{ ...idle,
      dictation: { status: "recording", recordingId: "new", audioPath: "new.wav" },
      liveTranscript: { recordingId: "new", revision: 1, text: "Bieżące słowa.", status: "draft", clipboardReady: true },
    }} />);
    expect(screen.queryByText("Poprzednia wiadomość.")).not.toBeInTheDocument();
    expect(screen.queryByText(recordings[2].text!)).not.toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Wersja robocza" })).toHaveTextContent("Bieżące słowa.");
    view.rerender(<DictatePage {...props} snapshot={idle} />);
    expect(screen.getByText("Poprzednia wiadomość.")).toBeVisible();
    expect(screen.queryByText("Bieżące słowa.")).not.toBeInTheDocument();
  });

  test("oba wpisy mają własne kopiowanie i otwierają pełny tekst", async () => {
    const user = userEvent.setup();
    const writeText = vi.spyOn(navigator.clipboard, "writeText");
    const onHistory = vi.fn();
    const items = [recordings[2], { ...recordings[2], id: "older", audioPath: null, text: "Długi tekst. ".repeat(100) }];
    renderWithI18n(<DictatePage adapter={adapterStub()} snapshot={{ settings, modelLoading: false, dictation: { status: "idle" } }} recordings={items} onSnapshot={vi.fn()} onHistory={onHistory} onToast={vi.fn()} />);
    const older = within(screen.getAllByRole("article")[1]);
    await user.click(older.getByRole("button", { name: "Kopiuj" }));
    expect(writeText).toHaveBeenCalledWith(items[1].text);
    await user.click(older.getByRole("button", { name: /Długi tekst/ }));
    expect(onHistory).toHaveBeenCalledWith("older");
    expect(older.queryByRole("button", { name: "Odtwórz" })).not.toBeInTheDocument();
  });

  test("Cofnij przepisuje zachowane nagranie i nie włącza mikrofonu", async () => {
    const retryTranscription = vi.fn(async () => ({ dictation: { status: "processing" as const, recordingId: "retained", audioPath: "retained.wav" }, settings, modelLoading: false }));
    const startRecording = vi.fn();
    renderState({ status: "cancelling", recordingId: "retained", audioPath: "retained.wav" }, { retryTranscription, startRecording });
    await userEvent.click(screen.getByRole("button", { name: "Przywróć nagranie i przepisz" }));
    expect(retryTranscription).toHaveBeenCalledWith("retained");
    expect(startRecording).not.toHaveBeenCalled();
    expect(screen.queryByLabelText("Czas nagrania")).not.toBeInTheDocument();
  });

  test("anuluje przetwarzanie przez tę samą komendę co nagranie", async () => {
    const cancelRecording = vi.fn(async () => ({ dictation: { status: "idle" as const }, settings, modelLoading: false }));
    renderState({ status: "processing", recordingId: "r", audioPath: "r.wav" }, { cancelRecording });
    await userEvent.click(screen.getByRole("button", { name: "Anuluj nagranie" }));
    expect(cancelRecording).toHaveBeenCalledOnce();
  });

  test("odtwarza ostatnie nagranie przez adapter bez przechodzenia do historii", async () => {
    const user = userEvent.setup();
    const getRecordingAudio = vi.fn(async () => new Uint8Array([1, 2, 3]));
    const createObjectURL = vi.fn(() => "blob:recent-audio");
    const revokeObjectURL = vi.fn();
    Object.defineProperty(URL, "createObjectURL", { value: createObjectURL, configurable: true });
    Object.defineProperty(URL, "revokeObjectURL", { value: revokeObjectURL, configurable: true });
    const play = vi.spyOn(HTMLMediaElement.prototype, "play").mockResolvedValue();
    const onHistory = vi.fn();
    const view = renderWithI18n(<DictatePage adapter={adapterStub({ getRecordingAudio })} snapshot={{ settings, modelLoading: false, dictation: { status: "idle" } }} recordings={recordings} onSnapshot={vi.fn()} onHistory={onHistory} onToast={vi.fn()} />);
    expect(getRecordingAudio).not.toHaveBeenCalled();
    expect(view.container.querySelector(".home-excerpt__audio--expanded")).toBeNull();
    await user.click(screen.getByRole("button", { name: "Odtwórz" }));
    expect(view.container.querySelector(".home-excerpt__audio--expanded")).not.toBeNull();
    expect(getRecordingAudio).toHaveBeenCalledWith("complete-1");
    expect(play).toHaveBeenCalledOnce();
    expect(onHistory).not.toHaveBeenCalled();
    view.unmount();
    expect(revokeObjectURL).toHaveBeenCalledWith("blob:recent-audio");
    play.mockRestore();
  });

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
      snapshot={{ dictation: { status: "idle" }, settings, model: { key: "parakeet", display: "Parakeet TDT 0.6B v3", provider: "NVIDIA", installed: false, totalBytes: 670_478_772, source: "local" }, modelLoading: false }}
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

  test("w trybie chmury bez klucza odsyła do ustawień, a nie po model", async () => {
    const onSettings = vi.fn();
    renderWithI18n(<DictatePage
      adapter={adapterStub()}
      snapshot={{
        dictation: { status: "idle" },
        settings: { ...settings, transcriptionSource: "cloud" },
        model: {
          key: "",
          display: "xAI (Grok) · grok-voice-transcribe-2.0",
          provider: "xAI (Grok)",
          installed: false,
          totalBytes: 0,
          source: "cloud",
        },
        modelLoading: false,
      }}
      recordings={[]}
      modelReady={false}
      onSnapshot={() => undefined}
      onHistory={() => undefined}
      onSettings={onSettings}
      onToast={() => undefined}
    />);

    expect(screen.getByRole("heading", { name: "Dodaj klucz API" })).toBeVisible();
    await userEvent.click(screen.getByRole("button", { name: "Otwórz ustawienia" }));

    expect(onSettings).toHaveBeenCalled();
    expect(screen.queryByRole("button", { name: "Pobierz model" })).toBeNull();
  });

  test("pobieranie zaczęte gdzie indziej widać po powrocie na ekran", async () => {
    // Stan pobierania należy do aplikacji, nie do ekranu. Trzymany w
    // komponencie znikał po wyjściu, a przycisk wracał gotowy do klikania —
    // i drugie pobieranie pisało po plikach pierwszego.
    const model = { key: "parakeet", display: "Parakeet TDT 0.6B v3", provider: "NVIDIA", installed: false, totalBytes: 670_478_772, source: "local" as const };
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
      snapshot={{ dictation: { status: "idle" }, settings, model: { key: "parakeet", display: "Parakeet TDT 0.6B v3", provider: "NVIDIA", installed: false, totalBytes: 670_478_772, source: "local" }, modelLoading: false }}
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

    await userEvent.click(screen.getByRole("button", { name: "Zobacz historię" }));
    expect(onHistory).toHaveBeenCalledOnce();
    expect(onHistory.mock.calls[0]).toEqual([]);
  });
});
