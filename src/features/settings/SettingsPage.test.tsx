import { fireEvent, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";

import { SettingsPage } from "./SettingsPage";
import { adapterStub, settings } from "../../test/fixtures";
import { renderWithI18n } from "../../test/renderWithI18n";

describe("ustawienia", () => {
  test("cofa nieudaną zmianę i pokazuje komunikat", async () => {
    const onToast = vi.fn();
    const updateSettings = vi.fn(async () => { throw new Error("Brak uprawnień"); });
    renderWithI18n(<SettingsPage adapter={adapterStub({ updateSettings })} initialSettings={settings} onToast={onToast} />);
    const toggle = screen.getByRole("checkbox", { name: "Wklejaj automatycznie" });
    await userEvent.click(toggle);
    expect(toggle).toBeChecked();
    expect(onToast).toHaveBeenCalledWith(expect.stringContaining("Brak uprawnień"), "error");
  });

  test("start w zasobniku zapisuje się jak każdy inny przełącznik", async () => {
    const updateSettings = vi.fn(async (next: typeof settings) => ({ settings: next, warning: null }));
    renderWithI18n(
      <SettingsPage adapter={adapterStub({ updateSettings })} initialSettings={settings} onToast={() => undefined} />,
    );

    await userEvent.click(screen.getByRole("checkbox", { name: /Startuj w zasobniku/ }));

    await waitFor(() => expect(updateSettings).toHaveBeenLastCalledWith(
      expect.objectContaining({ startMinimized: true }),
    ));
  });

  test("po każdej zmianie odświeża ustawienia i nie nadpisuje poprzedniej", async () => {
    let persisted = { ...settings };
    const updateSettings = vi.fn(async (next: typeof settings) => {
      persisted = { ...next };
      return { settings: persisted, warning: null };
    });
    const getSettings = vi.fn(async () => ({ ...persisted }));
    const onSettingsChange = vi.fn();
    const adapter = adapterStub({ updateSettings, getSettings });
    const first = renderWithI18n(
      <SettingsPage
        adapter={adapter}
        initialSettings={settings}
        onSettingsChange={onSettingsChange}
        onToast={() => undefined}
      />,
    );

    await userEvent.click(screen.getByRole("checkbox", { name: "Wklejaj automatycznie" }));
    await waitFor(() => expect(getSettings).toHaveBeenCalledTimes(1));
    await userEvent.click(screen.getByRole("checkbox", { name: /Uruchamiaj z systemem/ }));
    await waitFor(() => expect(getSettings).toHaveBeenCalledTimes(2));

    expect(updateSettings).toHaveBeenLastCalledWith(expect.objectContaining({
      autoPaste: false,
      launchOnLogin: false,
    }));
    expect(onSettingsChange).toHaveBeenLastCalledWith(expect.objectContaining({
      autoPaste: false,
      launchOnLogin: false,
    }));

    first.unmount();
    renderWithI18n(
      <SettingsPage
        adapter={adapter}
        initialSettings={persisted}
        onSettingsChange={() => undefined}
        onToast={() => undefined}
      />,
    );
    expect(screen.getByRole("checkbox", { name: "Wklejaj automatycznie" })).not.toBeChecked();
    expect(screen.getByRole("checkbox", { name: /Uruchamiaj z systemem/ })).not.toBeChecked();
  });

  test.each([
    ["ready", "Gotowy"],
    ["not_installed", "Do pobrania"],
    ["error", "Błąd"],
  ] as const)("pokazuje prawdziwy status modelu %s", async (state, label) => {
    renderWithI18n(<SettingsPage
      adapter={adapterStub({
        getModelStatus: async () => ({
          state,
          model: "nvidia/parakeet-tdt-0.6b-v3",
          revision: "revision",
          device: null,
          message: state === "error" ? "Cache niedostępny" : state === "not_installed" ? "Brakujące lub puste pliki: config.json." : null,
        }),
      })}
      initialSettings={settings}
      onSettingsChange={() => undefined}
      onToast={() => undefined}
    />);
    expect(screen.getByText("Sprawdzam…")).toBeVisible();
    expect(await screen.findByText(label)).toBeVisible();
    if (state === "not_installed") {
      expect(screen.getByText("Brakujące lub puste pliki: config.json.")).toBeVisible();
    }
  });

  test("pokazuje błąd listy mikrofonów", async () => {
    const listInputDevices = vi.fn().mockRejectedValueOnce(new Error("Brak dostępu"));
    renderWithI18n(<SettingsPage
      adapter={adapterStub({ listInputDevices })}
      initialSettings={settings}
      onSettingsChange={() => undefined}
      onToast={() => undefined}
    />);
    expect(await screen.findByText(/Nie udało się wczytać mikrofonów/)).toBeVisible();
  });

  test("nagrywa nowy skrót przez wciśnięcie kombinacji", async () => {
    const updateSettings = vi.fn(async (next: typeof settings) => ({ settings: next, warning: null }));
    const setShortcutSuspended = vi.fn(async () => undefined);
    renderWithI18n(<SettingsPage adapter={adapterStub({ updateSettings, setShortcutSuspended })} initialSettings={settings} onToast={() => undefined} />);

    await userEvent.click(screen.getByRole("button", { name: /Zmień globalny skrót/ }));
    expect(await screen.findByText("Naciśnij nowy skrót…")).toBeVisible();
    expect(setShortcutSuspended).toHaveBeenCalledWith(true);

    fireEvent.keyDown(window, { key: "k", ctrlKey: true });

    await waitFor(() => expect(updateSettings).toHaveBeenCalledWith(expect.objectContaining({ shortcut: "Ctrl+K" })));
    await waitFor(() => expect(setShortcutSuspended).toHaveBeenLastCalledWith(false));
  });

  test("esc anuluje nagrywanie skrótu bez zapisu", async () => {
    const updateSettings = vi.fn(async (next: typeof settings) => ({ settings: next, warning: null }));
    renderWithI18n(<SettingsPage adapter={adapterStub({ updateSettings })} initialSettings={settings} onToast={() => undefined} />);

    await userEvent.click(screen.getByRole("button", { name: /Zmień globalny skrót/ }));
    fireEvent.keyDown(window, { key: "Escape" });

    expect(updateSettings).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: /Zmień globalny skrót/ })).toBeEnabled();
  });

  test("wybór z listy rozwijanej zapisuje nową wartość", async () => {
    const updateSettings = vi.fn(async (next: typeof settings) => ({ settings: next, warning: null }));
    renderWithI18n(<SettingsPage
      adapter={adapterStub({ updateSettings })}
      initialSettings={settings}
      onSettingsChange={() => undefined}
      onToast={() => undefined}
    />);

    await userEvent.click(screen.getByRole("combobox", { name: "Przechowuj nagrania" }));
    await userEvent.click(await screen.findByRole("option", { name: "7 dni" }));

    await waitFor(() => expect(updateSettings).toHaveBeenCalledWith(
      expect.objectContaining({ retentionDays: 7 }),
    ));
  });

  test("wybór domyślnego mikrofonu zapisuje brak urządzenia, a nie pusty tekst", async () => {
    // Lista rozwijana rezerwuje pusty string na "brak wyboru", więc opcja
    // domyślna niesie wartość zastępczą, którą trzeba odwzorować na null.
    const updateSettings = vi.fn(async (next: typeof settings) => ({ settings: next, warning: null }));
    renderWithI18n(<SettingsPage
      adapter={adapterStub({ updateSettings })}
      initialSettings={{ ...settings, inputDevice: "default" }}
      onSettingsChange={() => undefined}
      onToast={() => undefined}
    />);

    await userEvent.click(await screen.findByRole("combobox", { name: "Mikrofon" }));
    await userEvent.click(await screen.findByRole("option", { name: "Domyślny systemowy" }));

    await waitFor(() => expect(updateSettings).toHaveBeenCalledWith(
      expect.objectContaining({ inputDevice: null }),
    ));
  });

  test("zmiana jednego ustawienia nie wyszarza pozostałych kontrolek", async () => {
    // Zapis był kiedyś blokujący: jeden przełącznik gasił wszystkie inne pola
    // na czas zapisu, co wyglądało jak migotanie całej strony.
    let release!: () => void;
    const updateSettings = vi.fn(async (next: typeof settings) => {
      await new Promise<void>((resolve) => { release = resolve; });
      return { settings: next, warning: null };
    });
    renderWithI18n(<SettingsPage
      adapter={adapterStub({ updateSettings })}
      initialSettings={settings}
      onSettingsChange={() => undefined}
      onToast={() => undefined}
    />);

    await userEvent.click(screen.getByRole("checkbox", { name: "Wklejaj automatycznie" }));

    await waitFor(() => expect(updateSettings).toHaveBeenCalled());
    expect(screen.getByRole("checkbox", { name: "Pokaż nakładkę" })).toBeEnabled();
    expect(screen.getByRole("combobox", { name: "Przechowuj nagrania" })).toBeEnabled();
    expect(screen.getByRole("combobox", { name: "Trzymaj model w pamięci" })).toBeEnabled();
    release();
  });

  test("dwie szybkie zmiany zapisują się obie", async () => {
    // Bez blokowania kontrolek dwa kliknięcia mogą się wyprzedzić, więc zapis
    // składa się na najnowszym stanie, a nie na tym z chwili renderowania.
    const saved: Array<typeof settings> = [];
    const updateSettings = vi.fn(async (next: typeof settings) => {
      saved.push(next);
      return { settings: next, warning: null };
    });
    renderWithI18n(<SettingsPage
      adapter={adapterStub({ updateSettings, getSettings: async () => saved[saved.length - 1] ?? settings })}
      initialSettings={settings}
      onSettingsChange={() => undefined}
      onToast={() => undefined}
    />);

    await userEvent.click(screen.getByRole("checkbox", { name: "Wklejaj automatycznie" }));
    await userEvent.click(screen.getByRole("checkbox", { name: "Uruchamiaj z systemem" }));

    await waitFor(() => expect(saved.length).toBe(2));
    expect(saved[1]).toEqual(expect.objectContaining({ autoPaste: false, launchOnLogin: false }));
  });

  test("wybór chmury zapisuje silnik i pokazuje dostawcę z domyślnym modelem", async () => {
    const adapter = adapterStub();
    const updateSettings = vi.spyOn(adapter, "updateSettings");
    renderWithI18n(<SettingsPage adapter={adapter} initialSettings={settings} onToast={() => undefined} />);

    await userEvent.click(screen.getByRole("radio", { name: "Chmura (API)" }));

    await waitFor(() => expect(updateSettings).toHaveBeenCalledWith(
      expect.objectContaining({ transcriptionSource: "cloud" }),
    ));
    expect(await screen.findByRole("combobox", { name: "Dostawca" })).toHaveTextContent("xAI (Grok)");
    // The model field falls back to what the provider calls its current
    // model, without storing it until the user edits it.
    expect(await screen.findByRole("combobox", { name: "Model" })).toHaveTextContent("grok-voice-transcribe-2.0");
  });

  test("zmiana dostawcy przestawia model na jego domyślny", async () => {
    const adapter = adapterStub();
    const updateSettings = vi.spyOn(adapter, "updateSettings");
    renderWithI18n(<SettingsPage
      adapter={adapter}
      initialSettings={{ ...settings, transcriptionSource: "cloud" }}
      onToast={() => undefined}
    />);

    await userEvent.click(await screen.findByRole("combobox", { name: "Dostawca" }));
    await userEvent.click(await screen.findByRole("option", { name: "OpenAI" }));

    await waitFor(() => expect(updateSettings).toHaveBeenCalledWith(
      expect.objectContaining({ cloudProvider: "openai", cloudModel: "gpt-transcribe" }),
    ));
  });

  test("klucz API zapisuje się poza ustawieniami i zaraz po zapisie go nie ma w polu", async () => {
    const setCloudApiKey = vi.fn(async () => ["xai"]);
    const onToast = vi.fn();
    renderWithI18n(<SettingsPage
      adapter={adapterStub({ setCloudApiKey })}
      initialSettings={{ ...settings, transcriptionSource: "cloud" }}
      onToast={onToast}
    />);

    const field = screen.getByLabelText("Klucz API");
    await userEvent.type(field, "xai-secret");
    await userEvent.click(screen.getByRole("button", { name: "Zapisz" }));

    await waitFor(() => expect(setCloudApiKey).toHaveBeenCalledWith("xai", "xai-secret"));
    expect(field).toHaveValue("");
    expect(onToast).toHaveBeenCalledWith("Klucz API zapisany.", "success");
  });

  test("test połączenia pokazuje odpowiedź dostawcy i czas", async () => {
    const testCloudTranscription = vi.fn(async () => ({ text: "Dzień dobry", elapsedMs: 1234 }));
    const onToast = vi.fn();
    renderWithI18n(<SettingsPage
      adapter={adapterStub({ listCloudKeys: async () => ["xai"], testCloudTranscription })}
      initialSettings={{ ...settings, transcriptionSource: "cloud" }}
      onToast={onToast}
    />);

    const button = await screen.findByRole("button", { name: "Przetestuj" });
    await waitFor(() => expect(button).toBeEnabled());
    await userEvent.click(button);

    await waitFor(() => expect(onToast).toHaveBeenCalledWith(
      "Połączenie działa w 1.2 s. Odpowiedź: „Dzień dobry”",
      "success",
    ));
    expect(testCloudTranscription).toHaveBeenCalled();
  });

  test("model można wpisać ręcznie, gdy lista go nie zawiera", async () => {
    const adapter = adapterStub();
    const updateSettings = vi.spyOn(adapter, "updateSettings");
    renderWithI18n(<SettingsPage
      adapter={adapter}
      initialSettings={{ ...settings, transcriptionSource: "cloud" }}
      onToast={() => undefined}
    />);

    await userEvent.click(await screen.findByRole("combobox", { name: "Model" }));
    await userEvent.click(await screen.findByRole("option", { name: "Własny…" }));

    const field = screen.getByLabelText("Model");
    await userEvent.clear(field);
    await userEvent.type(field, "grok-voice-transcribe-1.0{Enter}");

    await waitFor(() => expect(updateSettings).toHaveBeenCalledWith(
      expect.objectContaining({ cloudModel: "grok-voice-transcribe-1.0" }),
    ));
  });

  test("listę modeli można pobrać od dostawcy", async () => {
    const listCloudModels = vi.fn(async () => ["whisper-1", "gpt-transcribe"]);
    const onToast = vi.fn();
    renderWithI18n(<SettingsPage
      adapter={adapterStub({ listCloudKeys: async () => ["xai"], listCloudModels })}
      initialSettings={{ ...settings, transcriptionSource: "cloud" }}
      onToast={onToast}
    />);

    const button = await screen.findByRole("button", { name: "Pobierz listę modeli" });
    await waitFor(() => expect(button).toBeEnabled());
    await userEvent.click(button);

    await waitFor(() => expect(onToast).toHaveBeenCalledWith("Pobrano 2 modeli.", "success"));
    await userEvent.click(screen.getByRole("combobox", { name: "Model" }));
    expect(await screen.findByRole("option", { name: "whisper-1" })).toBeVisible();
  });

  test("tryb na żywo jest dostępny tylko u dostawcy, który go obsługuje", async () => {
    const adapter = adapterStub();
    const updateSettings = vi.spyOn(adapter, "updateSettings");
    const { rerender } = renderWithI18n(<SettingsPage
      adapter={adapter}
      initialSettings={{ ...settings, transcriptionSource: "cloud", streaming: false }}
      onToast={() => undefined}
    />);

    const live = await screen.findByRole("checkbox", { name: "Pisz na żywo" });
    await waitFor(() => expect(live).toBeEnabled());
    expect(live).not.toBeChecked();
    await userEvent.click(live);

    await waitFor(() => expect(updateSettings).toHaveBeenCalledWith(
      expect.objectContaining({ streaming: true }),
    ));

    rerender(<SettingsPage
      adapter={adapterStub()}
      initialSettings={{ ...settings, transcriptionSource: "cloud", cloudProvider: "openai" }}
      onToast={() => undefined}
    />);

    await waitFor(() => expect(screen.getByRole("checkbox", { name: "Pisz na żywo" })).toBeDisabled());
    expect(screen.getByText("Ten dostawca nie obsługuje trybu na żywo")).toBeVisible();
  });

  test("dostawca własny prosi o adres API, a znany go nie pokazuje", async () => {
    const { rerender } = renderWithI18n(<SettingsPage
      adapter={adapterStub()}
      initialSettings={{ ...settings, transcriptionSource: "cloud", cloudProvider: "custom" }}
      onToast={() => undefined}
    />);

    expect(await screen.findByLabelText("Adres API")).toBeVisible();

    rerender(<SettingsPage
      adapter={adapterStub()}
      initialSettings={{ ...settings, transcriptionSource: "cloud", cloudProvider: "xai" }}
      onToast={() => undefined}
    />);

    await waitFor(() => expect(screen.queryByLabelText("Adres API")).toBeNull());
  });

  test("rozmiar nakładki zapisuje się obok pokazywania nakładki i domyślnie jest kompaktowy", async () => {
    const updateSettings = vi.fn(async (next: typeof settings) => ({ settings: next, warning: null }));
    renderWithI18n(
      <SettingsPage adapter={adapterStub({ updateSettings })} initialSettings={settings} onToast={() => undefined} />,
    );

    expect(screen.getByRole("combobox", { name: "Rozmiar nakładki" })).toHaveTextContent("Kompaktowa");
    await userEvent.click(screen.getByRole("combobox", { name: "Rozmiar nakładki" }));
    await userEvent.click(await screen.findByRole("option", { name: "Rozszerzona" }));

    await waitFor(() => expect(updateSettings).toHaveBeenCalledWith(
      expect.objectContaining({ overlaySize: "large", showOverlay: true }),
    ));
  });

});
