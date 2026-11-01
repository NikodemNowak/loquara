import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";

import { SettingsPage } from "./SettingsPage";
import { adapterStub, settings } from "../../test/fixtures";
import { renderWithI18n } from "../../test/renderWithI18n";

describe("ustawienia", () => {
  test("microphone favorite order, unavailable placeholders and hiding can be reversed", async () => {
    const preferences = { selection: "manual" as const, selectedId: "usb", favorites: [{ id: "lost", name: "Old USB" }, { id: "usb", name: "USB" }], hiddenIds: ["lost"], allowSystemDefaultFallback: false, unresolvedLegacyName: null };
    const adapter = adapterStub({ listInputDevices: async () => [{ id: "usb", name: "USB", isDefault: true, available: true }] });
    const persisted = { ...settings, microphones: preferences, inputDevice: "usb" };
    await adapter.updateSettings(persisted);
    renderWithI18n(<SettingsPage adapter={adapter} initialSettings={persisted} onToast={vi.fn()} />, { language: "en" });
    await userEvent.click(screen.getByText("Favorites and hidden microphones"));
    expect(await screen.findByRole("button", { name: "Hide microphone: USB" })).toBeDisabled();
    expect(screen.getAllByText(/Old USB.*Unavailable/).length).toBeGreaterThan(0);
    await userEvent.click(screen.getByRole("button", { name: "Move up: USB" }));
    await waitFor(async () => expect((await adapter.getSettings()).microphones?.favorites.map((item) => item.id)).toEqual(["usb", "lost"]));
    await userEvent.click(screen.getByRole("button", { name: "Restore microphone: Old USB" }));
    await waitFor(async () => expect((await adapter.getSettings()).microphones?.hiddenIds).toEqual([]));
    await userEvent.click(screen.getByRole("button", { name: "Remove favorite: Old USB" }));
    await waitFor(async () => expect((await adapter.getSettings()).microphones?.favorites.map((item) => item.id)).toEqual(["usb"]));
  });

  test("device events refresh a missing selected endpoint and unsubscribe on departure", async () => {
    let changed!: Parameters<ReturnType<typeof adapterStub>["onInputDevicesChanged"]>[0];
    const dispose = vi.fn();
    const list = vi.fn().mockResolvedValueOnce([{ id: "usb", name: "USB", isDefault: true, available: true }]).mockResolvedValue([]);
    const adapter = adapterStub({ listInputDevices: list, onInputDevicesChanged: async (listener) => { changed = listener; return dispose; } });
    const initial = { ...settings, microphones: { selection: "manual" as const, selectedId: "usb", favorites: [{ id: "usb", name: "USB" }], hiddenIds: [], allowSystemDefaultFallback: false, unresolvedLegacyName: null } };
    const view = renderWithI18n(<SettingsPage adapter={adapter} initialSettings={initial} onToast={vi.fn()} />, { language: "en" });
    await waitFor(() => expect(screen.getByRole("combobox", { name: "Microphone" })).toHaveTextContent("USB"));
    act(() => changed({ generation: 1, devices: [] }));
    await waitFor(() => expect(screen.getByRole("combobox", { name: "Microphone" })).toHaveTextContent("USB — Unavailable"));
    expect(list).toHaveBeenCalledTimes(2);
    view.unmount();
    expect(dispose).toHaveBeenCalledOnce();
  });
  test("microphone favorites stay manual until automatic selection is explicitly enabled", async () => {
    const adapter = adapterStub({ listInputDevices: async () => [{ id: "usb", name: "Mikrofon USB", isDefault: true, available: true }] });
    const update = vi.spyOn(adapter, "updateSettings");
    renderWithI18n(<SettingsPage adapter={adapter} initialSettings={settings} onToast={vi.fn()} />, { language: "en" });
    await userEvent.click(screen.getByText("Favorites and hidden microphones"));
    await userEvent.click(await screen.findByRole("button", { name: "Add favorite: Mikrofon USB" }));
    await waitFor(() => expect(update).toHaveBeenLastCalledWith(expect.objectContaining({ microphones: expect.objectContaining({ selection: "manual", favorites: [{ id: "usb", name: "Mikrofon USB" }] }) })));
    await userEvent.click(screen.getByRole("checkbox", { name: "Choose the first available favorite" }));
    await waitFor(() => expect(update).toHaveBeenLastCalledWith(expect.objectContaining({ microphones: expect.objectContaining({ selection: "favorites", allowSystemDefaultFallback: false }) })));
  });
  test("starsze ustawienia domyślnie pokazują przełączanie", () => {
    const { activationMode: _legacyMode, ...legacy } = settings;
    renderWithI18n(<SettingsPage adapter={adapterStub()} initialSettings={legacy} onToast={vi.fn()} />);
    expect(screen.getByRole("combobox", { name: "Działanie skrótu" })).toHaveTextContent("Przełączanie");
    expect(screen.getByText("Naciśnij raz, aby nagrywać, drugi raz, aby zakończyć.")).toBeVisible();
  });

  test("zapisuje oba tryby i zachowuje wybrany skrót po powrocie do ustawień", async () => {
    const initialSettings = { ...settings, shortcut: "Ctrl+Shift+M" };
    const adapter = adapterStub();
    await adapter.updateSettings(initialSettings);
    const updateSettings = vi.spyOn(adapter, "updateSettings");
    const onSettingsChange = vi.fn();
    const props = { adapter, onSettingsChange, onToast: vi.fn() };
    const view = renderWithI18n(<SettingsPage {...props} initialSettings={initialSettings} />);
    await userEvent.click(screen.getByRole("combobox", { name: "Działanie skrótu" }));
    await userEvent.click(await screen.findByRole("option", { name: "Przytrzymanie" }));
    await waitFor(() => expect(onSettingsChange).toHaveBeenLastCalledWith(expect.objectContaining({ activationMode: "push_to_talk", shortcut: "Ctrl+Shift+M" })));
    expect(screen.getByText("Przytrzymaj skrót, aby nagrywać, puść, aby zakończyć.")).toBeVisible();
    const persisted = await adapter.getSettings();
    view.unmount();
    renderWithI18n(<SettingsPage {...props} initialSettings={persisted} />);
    expect(screen.getByRole("combobox", { name: "Działanie skrótu" })).toHaveTextContent("Przytrzymanie");
    await userEvent.click(screen.getByRole("combobox", { name: "Działanie skrótu" }));
    await userEvent.click(await screen.findByRole("option", { name: "Przełączanie" }));
    await waitFor(() => expect(updateSettings).toHaveBeenLastCalledWith(expect.objectContaining({ activationMode: "toggle", shortcut: "Ctrl+Shift+M" })));
  });

  test("cofa tryb i jego pomoc po odrzuceniu zapisu", async () => {
    let rejectSave!: (error: Error) => void;
    const updateSettings = vi.fn(() => new Promise<never>((_resolve, reject) => { rejectSave = reject; }));
    const onToast = vi.fn();
    const onSettingsChange = vi.fn();
    renderWithI18n(<SettingsPage adapter={adapterStub({ updateSettings })} initialSettings={settings} onToast={onToast} onSettingsChange={onSettingsChange} />);
    await userEvent.click(screen.getByRole("combobox", { name: "Działanie skrótu" }));
    await userEvent.click(await screen.findByRole("option", { name: "Przytrzymanie" }));
    expect(screen.getByRole("combobox", { name: "Działanie skrótu" })).toHaveTextContent("Przytrzymanie");
    await act(async () => rejectSave(new Error("Skrót niedostępny")));
    expect(screen.getByRole("combobox", { name: "Działanie skrótu" })).toHaveTextContent("Przełączanie");
    expect(screen.getByText("Naciśnij raz, aby nagrywać, drugi raz, aby zakończyć.")).toBeVisible();
    expect(onToast).toHaveBeenCalledWith(expect.stringContaining("Skrót niedostępny"), "error");
    expect(onSettingsChange).not.toHaveBeenCalled();
  });

  test("tryb przytrzymania ma angielską etykietę i pomoc", () => {
    renderWithI18n(<SettingsPage adapter={adapterStub()} initialSettings={{ ...settings, activationMode: "push_to_talk" }} onToast={vi.fn()} />, { language: "en" });
    expect(screen.getByRole("combobox", { name: "Shortcut behavior" })).toHaveTextContent("Hold to talk");
    expect(screen.getByText("Hold the shortcut to record, release it to finish.")).toBeVisible();
  });

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

    await userEvent.click(screen.getByRole("tab", { name: "Aplikacja" }));
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
    await userEvent.click(screen.getByRole("tab", { name: "Aplikacja" }));
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
    await userEvent.click(screen.getByRole("tab", { name: "Aplikacja" }));
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
    expect(screen.getAllByText("Sprawdzam…").length).toBeGreaterThan(0);
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

    await userEvent.click(screen.getByRole("tab", { name: "Aplikacja" }));
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
    await userEvent.click(screen.getByRole("tab", { name: "Aplikacja" }));
    expect(screen.getByRole("combobox", { name: "Przechowuj nagrania" })).toBeEnabled();
    expect(screen.queryByRole("combobox", { name: "Trzymaj model w pamięci" })).toBeNull();
    await userEvent.click(screen.getByRole("tab", { name: "Modele i API" }));
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
    await userEvent.click(screen.getByRole("tab", { name: "Aplikacja" }));
    await userEvent.click(screen.getByRole("checkbox", { name: "Uruchamiaj z systemem" }));

    await waitFor(() => expect(saved.length).toBe(2));
    expect(saved[1]).toEqual(expect.objectContaining({ autoPaste: false, launchOnLogin: false }));
  });

  test("wybór chmury zapisuje silnik i pokazuje dostawcę z domyślnym modelem", async () => {
    const adapter = adapterStub();
    const updateSettings = vi.spyOn(adapter, "updateSettings");
    renderWithI18n(<SettingsPage adapter={adapter} initialSettings={settings} onToast={() => undefined} />);

    await userEvent.click(screen.getByRole("tab", { name: "Modele i API" }));
    await userEvent.click(screen.getByRole("radio", { name: "Chmura (API)" }));

    await waitFor(() => expect(updateSettings).toHaveBeenCalledWith(
      expect.objectContaining({ transcriptionSource: "cloud" }),
    ));
    expect(await screen.findByRole("combobox", { name: "Dostawca" })).toHaveTextContent("xAI (Grok)");
    // The model field falls back to what the provider calls its current
    // model, without storing it until the user edits it.
    expect(await screen.findByRole("combobox", { name: "Model" })).toHaveTextContent("grok-voice-transcribe-2.0");
    // xAI does not publish its speech models, so it is not asked for them.
    expect(screen.queryByRole("button", { name: "Pobierz listę modeli" })).toBeNull();
  });

  test("zmiana dostawcy przestawia model na jego domyślny", async () => {
    const adapter = adapterStub();
    const updateSettings = vi.spyOn(adapter, "updateSettings");
    renderWithI18n(<SettingsPage
      adapter={adapter}
      initialSettings={{ ...settings, transcriptionSource: "cloud" }}
      onToast={() => undefined}
    />);

    await userEvent.click(screen.getByRole("tab", { name: "Modele i API" }));
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

    await userEvent.click(screen.getByRole("tab", { name: "Modele i API" }));
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

    await userEvent.click(screen.getByRole("tab", { name: "Modele i API" }));
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

    await userEvent.click(screen.getByRole("tab", { name: "Modele i API" }));
    await userEvent.click(await screen.findByRole("combobox", { name: "Model" }));
    await userEvent.click(await screen.findByRole("option", { name: "Własny…" }));

    const field = screen.getByLabelText("Model");
    await userEvent.clear(field);
    await userEvent.type(field, "grok-voice-transcribe-1.0{Enter}");

    await waitFor(() => expect(updateSettings).toHaveBeenCalledWith(
      expect.objectContaining({ cloudModel: "grok-voice-transcribe-1.0" }),
    ));
  });

  test("listę modeli można pobrać od dostawcy, który ją publikuje", async () => {
    const listCloudModels = vi.fn(async () => ["gpt-4o-transcribe-diarize", "gpt-transcribe"]);
    const onToast = vi.fn();
    renderWithI18n(<SettingsPage
      adapter={adapterStub({ listCloudKeys: async () => ["openai"], listCloudModels })}
      initialSettings={{ ...settings, transcriptionSource: "cloud", cloudProvider: "openai", cloudModel: "" }}
      onToast={onToast}
    />);

    await userEvent.click(screen.getByRole("tab", { name: "Modele i API" }));
    const button = await screen.findByRole("button", { name: "Pobierz listę modeli" });
    await waitFor(() => expect(button).toBeEnabled());
    await userEvent.click(button);

    await waitFor(() => expect(onToast).toHaveBeenCalledWith("Pobrano 2 modeli.", "success"));
    await userEvent.click(screen.getByRole("combobox", { name: "Model" }));
    expect(await screen.findByRole("option", { name: "gpt-4o-transcribe-diarize" })).toBeVisible();
  });

  test("tryb na żywo jest dostępny tylko u dostawcy, który go obsługuje", async () => {
    const adapter = adapterStub();
    const updateSettings = vi.spyOn(adapter, "updateSettings");
    const { rerender } = renderWithI18n(<SettingsPage
      adapter={adapter}
      initialSettings={{ ...settings, transcriptionSource: "cloud", streaming: false }}
      onToast={() => undefined}
    />);

    const live = await screen.findByRole("checkbox", { name: "Podgląd na żywo" });
    await waitFor(() => expect(live).toBeEnabled());
    expect(live).not.toBeChecked();
    await userEvent.click(live);

    await waitFor(() => expect(updateSettings).toHaveBeenCalledWith(
      expect.objectContaining({ realtimeByEngine: expect.objectContaining({ "cloud:xai": true }) }),
    ));

    rerender(<SettingsPage
      adapter={adapterStub()}
      initialSettings={{ ...settings, transcriptionSource: "cloud", cloudProvider: "openai" }}
      onToast={() => undefined}
    />);

    await waitFor(() => expect(screen.getByRole("checkbox", { name: "Podgląd na żywo" })).toBeDisabled());
    expect(screen.getByText("Ten silnik przepisuje nagranie po zatrzymaniu.")).toBeVisible();
  });

  test("dostawca własny prosi o adres API, a znany go nie pokazuje", async () => {
    const { rerender } = renderWithI18n(<SettingsPage
      adapter={adapterStub()}
      initialSettings={{ ...settings, transcriptionSource: "cloud", cloudProvider: "custom" }}
      onToast={() => undefined}
    />);

    await userEvent.click(screen.getByRole("tab", { name: "Modele i API" }));
    expect(await screen.findByLabelText("Adres API")).toBeVisible();

    rerender(<SettingsPage
      adapter={adapterStub()}
      initialSettings={{ ...settings, transcriptionSource: "cloud", cloudProvider: "xai" }}
      onToast={() => undefined}
    />);

    await waitFor(() => expect(screen.queryByLabelText("Adres API")).toBeNull());
  });

  test("legacy expanded preference has no size control and next save normalizes compact", async () => {
    const updateSettings = vi.fn(async (next: typeof settings) => ({ settings: next, warning: null }));
    renderWithI18n(<SettingsPage adapter={adapterStub({ updateSettings })} initialSettings={{ ...settings, overlaySize: "large" }} onToast={() => undefined} />);
    expect(screen.queryByRole("combobox", { name: "Rozmiar nakładki" })).toBeNull();
    await userEvent.click(screen.getByRole("checkbox", { name: "Pokaż nakładkę" }));
    await waitFor(() => expect(updateSettings).toHaveBeenCalledWith(expect.objectContaining({ overlaySize: "mini", showOverlay: false })));
    for (const tab of ["Modele i API", "Aplikacja"]) {
      await userEvent.click(screen.getByRole("tab", { name: tab }));
      expect(screen.queryByRole("combobox", { name: "Rozmiar nakładki" })).toBeNull();
    }
  });

});


test("local realtime preference defaults on and persists independently from output and cloud", async () => {
  const initial = { ...settings, autoPaste: false, realtimeByEngine: { "cloud:xai": false } };
  const adapter = adapterStub({ getSettings: async () => initial });
  const updateSettings = vi.spyOn(adapter, "updateSettings");
  renderWithI18n(<SettingsPage adapter={adapter} initialSettings={initial} onToast={() => undefined} />);
  const preview = await screen.findByRole("checkbox", { name: "Podgląd na żywo" });
  expect(preview).toBeChecked();
  expect(screen.getByRole("checkbox", { name: "Wklejaj automatycznie" })).not.toBeChecked();
  await userEvent.click(preview);
  await waitFor(() => expect(updateSettings).toHaveBeenCalledWith(expect.objectContaining({
    autoPaste: false, realtimeByEngine: { "cloud:xai": false, "local:parakeet": false },
  })));
});

test("unsupported local model disables realtime despite remembered preferences", async () => {
  renderWithI18n(<SettingsPage adapter={adapterStub()} initialSettings={{ ...settings, model: "whisper-small", realtimeByEngine: { "local:whisper-small": true } }} onToast={() => undefined} />);
  const preview = await screen.findByRole("checkbox", { name: "Podgląd na żywo" });
  expect(preview).toBeDisabled();
  expect(preview).not.toBeChecked();
  expect(screen.getByText("Ten silnik przepisuje nagranie po zatrzymaniu.")).toBeVisible();
});


test("task tabs expose their controls once and support arrow/home/end navigation", async () => {
  renderWithI18n(<SettingsPage adapter={adapterStub()} initialSettings={settings} onToast={() => undefined} />);
  const dictation = screen.getByRole("tab", { name: "Dyktowanie" });
  expect(dictation).toHaveAttribute("aria-selected", "true");
  expect(screen.getByRole("checkbox", { name: "Podgląd na żywo" })).toBeVisible();
  expect(screen.queryByRole("radio", { name: "Chmura (API)" })).toBeNull();
  expect(screen.queryByRole("checkbox", { name: "Uruchamiaj z systemem" })).toBeNull();
  dictation.focus();
  await userEvent.keyboard("{ArrowRight}");
  expect(screen.getByRole("tab", { name: "Modele i API" })).toHaveFocus();
  expect(screen.getByRole("radio", { name: "Chmura (API)" })).toBeVisible();
  expect(screen.queryByRole("checkbox", { name: "Podgląd na żywo" })).toBeNull();
  await userEvent.keyboard("{End}");
  expect(screen.getByRole("tab", { name: "Aplikacja" })).toHaveFocus();
  expect(screen.getByRole("checkbox", { name: "Uruchamiaj z systemem" })).toBeVisible();
  expect(screen.queryByRole("radio", { name: "Chmura (API)" })).toBeNull();
  await userEvent.keyboard("{Home}");
  expect(dictation).toHaveFocus();
  expect(dictation).toHaveAttribute("aria-selected", "true");
});

test("active engine summary opens model management without changing saved engine", async () => {
  const updateSettings = vi.fn();
  renderWithI18n(<SettingsPage adapter={adapterStub({ updateSettings })} initialSettings={settings} onToast={() => undefined} />);
  await userEvent.click(screen.getByRole("button", { name: "Zarządzaj modelami" }));
  expect(screen.getByRole("tab", { name: "Modele i API" })).toHaveAttribute("aria-selected", "true");
  expect(await screen.findByRole("radiogroup", { name: "Wybierz silnik" })).toBeVisible();
  expect(updateSettings).not.toHaveBeenCalled();
});

test("microphone failure retries only devices and restores selection", async () => {
  const listInputDevices = vi.fn().mockRejectedValueOnce(new Error("Device permission")).mockResolvedValueOnce([{ id: "usb", name: "USB microphone", isDefault: true, available: true }]);
  const adapter = adapterStub({ listInputDevices });
  const listModels = vi.spyOn(adapter, "listModels");
  const listCloudProviders = vi.spyOn(adapter, "listCloudProviders");
  renderWithI18n(<SettingsPage adapter={adapter} initialSettings={settings} onToast={() => undefined} />);
  const error = await screen.findByRole("alert");
  expect(error).toHaveTextContent("Device permission");
  expect(screen.getByRole("combobox", { name: "Mikrofon" })).toBeDisabled();
  await userEvent.click(within(error).getByRole("button", { name: "Ponów" }));
  await waitFor(() => expect(screen.getByRole("combobox", { name: "Mikrofon" })).toBeEnabled());
  await userEvent.click(screen.getByRole("combobox", { name: "Mikrofon" }));
  expect(await screen.findByRole("option", { name: "USB microphone" })).toBeVisible();
  expect(listInputDevices).toHaveBeenCalledTimes(2);
  expect(listModels).toHaveBeenCalledTimes(1);
  expect(listCloudProviders).toHaveBeenCalledTimes(1);
  expect(screen.queryByRole("alert")).toBeNull();
});

test("model list loading, failure and empty are distinct and recover locally", async () => {
  const models = await adapterStub().listModels();
  let release!: (value: typeof models) => void;
  const listModels = vi.fn().mockRejectedValueOnce(new Error("Catalogue missing")).mockImplementationOnce(() => new Promise<typeof models>((resolve) => { release = resolve; })).mockResolvedValueOnce(models);
  const adapter = adapterStub({ listModels });
  const devices = vi.spyOn(adapter, "listInputDevices");
  const status = vi.spyOn(adapter, "getModelStatus");
  renderWithI18n(<SettingsPage adapter={adapter} initialSettings={settings} onToast={() => undefined} />);
  await userEvent.click(screen.getByRole("tab", { name: "Modele i API" }));
  const error = await screen.findByRole("alert");
  expect(error).toHaveTextContent("Catalogue missing");
  expect(screen.queryByText("Sprawdzam…")).toBeNull();
  await userEvent.click(within(error).getByRole("button", { name: "Ponów" }));
  expect(await screen.findByText("Sprawdzam…")).toBeVisible();
  await act(async () => release([]));
  const empty = screen.getByRole("status");
  expect(empty).toHaveTextContent("Brak dostępnych modeli.");
  await userEvent.click(within(empty).getByRole("button", { name: "Ponów" }));
  expect(await screen.findByRole("radio", { name: /Parakeet TDT/ })).toBeVisible();
  expect(listModels).toHaveBeenCalledTimes(3);
  expect(devices).toHaveBeenCalledTimes(1);
  expect(status).toHaveBeenCalledTimes(1);
});

test("local status retry refreshes readiness without refetching catalogues", async () => {
  const adapter = adapterStub();
  const ready = await adapter.getModelStatus();
  const getModelStatus = vi.spyOn(adapter, "getModelStatus").mockRejectedValueOnce(new Error("Model check failed")).mockResolvedValueOnce(ready);
  const listModels = vi.spyOn(adapter, "listModels");
  renderWithI18n(<SettingsPage adapter={adapter} initialSettings={settings} onToast={() => undefined} />);
  const error = await screen.findByRole("alert");
  expect(error).toHaveTextContent("Model check failed");
  await userEvent.click(within(error).getByRole("button", { name: "Ponów" }));
  expect(await screen.findByText("Gotowy")).toBeVisible();
  expect(screen.queryByRole("alert")).toBeNull();
  expect(getModelStatus).toHaveBeenCalledTimes(2);
  expect(listModels).toHaveBeenCalledTimes(1);
});

test("model memory preference belongs only to local models and persists", async () => {
  const adapter = adapterStub();
  const updateSettings = vi.spyOn(adapter, "updateSettings");
  renderWithI18n(<SettingsPage adapter={adapter} initialSettings={settings} onToast={() => undefined} />);
  expect(screen.queryByRole("combobox", { name: "Trzymaj model w pamięci" })).toBeNull();
  await userEvent.click(screen.getByRole("tab", { name: "Aplikacja" }));
  expect(screen.queryByRole("combobox", { name: "Trzymaj model w pamięci" })).toBeNull();
  await userEvent.click(screen.getByRole("tab", { name: "Modele i API" }));
  await userEvent.click(screen.getByRole("combobox", { name: "Trzymaj model w pamięci" }));
  await userEvent.click(await screen.findByRole("option", { name: "3 min" }));
  await waitFor(() => expect(updateSettings).toHaveBeenCalledWith(expect.objectContaining({ modelKeepAliveSecs: 180 })));
  await userEvent.click(screen.getByRole("radio", { name: "Chmura (API)" }));
  expect(screen.queryByRole("combobox", { name: "Trzymaj model w pamięci" })).toBeNull();
});

test("cloud provider loading failure has scoped recovery", async () => {
  const providers = await adapterStub().listCloudProviders();
  const listCloudProviders = vi.fn().mockRejectedValueOnce(new Error("Providers unavailable")).mockResolvedValueOnce(providers);
  const adapter = adapterStub({ listCloudProviders });
  const listCloudKeys = vi.spyOn(adapter, "listCloudKeys");
  renderWithI18n(<SettingsPage adapter={adapter} initialSettings={{ ...settings, transcriptionSource: "cloud" }} onToast={() => undefined} />);
  await userEvent.click(screen.getByRole("tab", { name: "Modele i API" }));
  const error = await screen.findByRole("alert");
  expect(error).toHaveTextContent("Providers unavailable");
  expect(screen.getByRole("combobox", { name: "Dostawca" })).toBeDisabled();
  await userEvent.click(within(error).getByRole("button", { name: "Ponów" }));
  await waitFor(() => expect(screen.getByRole("combobox", { name: "Dostawca" })).toBeEnabled());
  expect(screen.getByRole("combobox", { name: "Dostawca" })).toHaveTextContent("xAI (Grok)");
  expect(listCloudProviders).toHaveBeenCalledTimes(2);
  expect(listCloudKeys).toHaveBeenCalledTimes(1);
});

test("failed key lookup stays unknown until retry and never claims validation", async () => {
  const listCloudKeys = vi.fn().mockRejectedValueOnce(new Error("Key store locked")).mockResolvedValueOnce(["xai"]);
  const adapter = adapterStub({ listCloudKeys });
  const listCloudProviders = vi.spyOn(adapter, "listCloudProviders");
  const testCloudTranscription = vi.spyOn(adapter, "testCloudTranscription");
  renderWithI18n(<SettingsPage adapter={adapter} initialSettings={{ ...settings, transcriptionSource: "cloud" }} onToast={() => undefined} />);
  await userEvent.click(screen.getByRole("tab", { name: "Modele i API" }));
  const error = await screen.findByRole("alert");
  expect(error).toHaveTextContent("Key store locked");
  expect(screen.getByRole("button", { name: "Przetestuj" })).toBeDisabled();
  await userEvent.click(within(error).getByRole("button", { name: "Ponów" }));
  await waitFor(() => expect(screen.getByRole("button", { name: "Przetestuj" })).toBeEnabled());
  expect(screen.getByRole("button", { name: "Usuń klucz" })).toBeEnabled();
  expect(testCloudTranscription).not.toHaveBeenCalled();
  expect(listCloudKeys).toHaveBeenCalledTimes(2);
  expect(listCloudProviders).toHaveBeenCalledTimes(1);
});

test("cloud model fetch failure is inline and retry restores provider options", async () => {
  const listCloudModels = vi.fn().mockRejectedValueOnce(new Error("Provider timeout")).mockResolvedValueOnce(["new-model"]);
  const adapter = adapterStub({ listCloudModels, listCloudKeys: async () => ["openai"] });
  renderWithI18n(<SettingsPage adapter={adapter} initialSettings={{ ...settings, transcriptionSource: "cloud", cloudProvider: "openai" }} onToast={() => undefined} />);
  await userEvent.click(screen.getByRole("tab", { name: "Modele i API" }));
  const fetch = await screen.findByRole("button", { name: "Pobierz listę modeli" });
  await waitFor(() => expect(fetch).toBeEnabled());
  await userEvent.click(fetch);
  expect(await screen.findByRole("alert")).toHaveTextContent("Provider timeout");
  await userEvent.click(screen.getByRole("button", { name: "Ponów" }));
  await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
  await userEvent.click(screen.getByRole("combobox", { name: "Model" }));
  expect(await screen.findByRole("option", { name: "new-model" })).toBeVisible();
  expect(listCloudModels).toHaveBeenCalledTimes(2);
});

test("empty cloud model response is visible while manual selection stays available", async () => {
  const adapter = adapterStub({ listCloudModels: async () => [], listCloudKeys: async () => ["openai"] });
  renderWithI18n(<SettingsPage adapter={adapter} initialSettings={{ ...settings, transcriptionSource: "cloud", cloudProvider: "openai" }} onToast={() => undefined} />);
  await userEvent.click(screen.getByRole("tab", { name: "Modele i API" }));
  const fetch = await screen.findByRole("button", { name: "Pobierz listę modeli" });
  await waitFor(() => expect(fetch).toBeEnabled());
  await userEvent.click(fetch);
  expect(await screen.findByRole("status")).toHaveTextContent("Nie zwrócono modeli. Możesz wpisać identyfikator ręcznie.");
  await userEvent.click(screen.getByRole("combobox", { name: "Model" }));
  expect(await screen.findByRole("option", { name: "Własny…" })).toBeVisible();
});

test("late model list cannot contaminate another provider", async () => {
  let release!: (value: string[]) => void;
  const onToast = vi.fn();
  const adapter = adapterStub({ listCloudKeys: async () => ["openai", "xai"], listCloudModels: () => new Promise<string[]>((resolve) => { release = resolve; }) });
  renderWithI18n(<SettingsPage adapter={adapter} initialSettings={{ ...settings, transcriptionSource: "cloud", cloudProvider: "openai" }} onToast={onToast} />);
  await userEvent.click(screen.getByRole("tab", { name: "Modele i API" }));
  const fetch = await screen.findByRole("button", { name: "Pobierz listę modeli" });
  await waitFor(() => expect(fetch).toBeEnabled());
  await userEvent.click(fetch);
  expect(screen.getByRole("button", { name: "Pobieram…" })).toBeDisabled();
  await userEvent.click(screen.getByRole("combobox", { name: "Dostawca" }));
  await userEvent.click(await screen.findByRole("option", { name: "xAI (Grok)" }));
  await act(async () => release(["foreign-model"]));
  await userEvent.click(screen.getByRole("combobox", { name: "Model" }));
  expect(screen.queryByRole("option", { name: "foreign-model" })).toBeNull();
  expect(await screen.findByRole("option", { name: "grok-voice-transcribe-2.0" })).toBeVisible();
  expect(onToast).not.toHaveBeenCalled();
});

test("English settings expose the same preference groups and controls", async () => {
  renderWithI18n(<SettingsPage adapter={adapterStub()} initialSettings={settings} onToast={() => undefined} />, { language: "en" });
  expect(screen.getByRole("heading", { name: "Your dictation shortcut" })).toBeVisible();
  expect(screen.getByRole("heading", { name: "After dictation" })).toBeVisible();
  await userEvent.click(screen.getByRole("tab", { name: "Models & API" }));
  expect(screen.getByRole("combobox", { name: "Keep model in memory" })).toBeVisible();
});

test("custom API address and language save through their existing controls", async () => {
  const adapter = adapterStub();
  const updateSettings = vi.spyOn(adapter, "updateSettings");
  renderWithI18n(<SettingsPage adapter={adapter} initialSettings={{ ...settings, transcriptionSource: "cloud", cloudProvider: "custom" }} onToast={() => undefined} />);
  await userEvent.click(screen.getByRole("tab", { name: "Modele i API" }));
  const address = await screen.findByLabelText("Adres API");
  await userEvent.type(address, "https://example.test/v1{Enter}");
  await waitFor(() => expect(updateSettings).toHaveBeenCalledWith(expect.objectContaining({ cloudBaseUrl: "https://example.test/v1" })));
  await userEvent.click(screen.getByRole("combobox", { name: "Język" }));
  await userEvent.click(await screen.findByRole("option", { name: "Polski" }));
  await waitFor(() => expect(updateSettings).toHaveBeenLastCalledWith(expect.objectContaining({ cloudBaseUrl: "https://example.test/v1", cloudLanguage: "pl" })));
});

test("stored credentials require explicit removal and clear only that provider", async () => {
  const clearCloudApiKey = vi.fn(async () => ["openai"]);
  const adapter = adapterStub({ listCloudKeys: async () => ["xai", "openai"], clearCloudApiKey });
  renderWithI18n(<SettingsPage adapter={adapter} initialSettings={{ ...settings, transcriptionSource: "cloud" }} onToast={() => undefined} />);
  await userEvent.click(screen.getByRole("tab", { name: "Modele i API" }));
  const remove = await screen.findByRole("button", { name: "Usuń klucz" });
  expect(screen.getByLabelText("Klucz API")).toHaveValue("");
  expect(screen.getByRole("button", { name: "Zapisz" })).toBeDisabled();
  expect(clearCloudApiKey).not.toHaveBeenCalled();
  await userEvent.click(remove);
  await waitFor(() => expect(clearCloudApiKey).toHaveBeenCalledWith("xai"));
  expect(screen.queryByRole("button", { name: "Usuń klucz" })).toBeNull();
  expect(screen.getByRole("button", { name: "Przetestuj" })).toBeDisabled();
});

test("empty microphone discovery offers scoped retry and retains system default", async () => {
  const listInputDevices = vi.fn().mockResolvedValueOnce([]).mockResolvedValueOnce([{ id: "usb", name: "USB microphone", isDefault: true, available: true }]);
  const adapter = adapterStub({ listInputDevices });
  renderWithI18n(<SettingsPage adapter={adapter} initialSettings={settings} onToast={() => undefined} />);
  expect(await screen.findByText(/Nie znaleziono mikrofonów/)).toBeVisible();
  const empty = screen.getByRole("status");
  expect(screen.getByRole("combobox", { name: "Mikrofon" })).toHaveTextContent("Domyślny systemowy");
  await userEvent.click(within(empty).getByRole("button", { name: "Ponów" }));
  await waitFor(() => expect(screen.queryByText(/Nie znaleziono mikrofonów/)).toBeNull());
  await userEvent.click(screen.getByRole("combobox", { name: "Mikrofon" }));
  expect(await screen.findByRole("option", { name: "USB microphone" })).toBeVisible();
  expect(listInputDevices).toHaveBeenCalledTimes(2);
});

test("empty provider catalogue offers retry without pretending to load", async () => {
  const providers = await adapterStub().listCloudProviders();
  const listCloudProviders = vi.fn().mockResolvedValueOnce([]).mockResolvedValueOnce(providers);
  renderWithI18n(<SettingsPage adapter={adapterStub({ listCloudProviders })} initialSettings={{ ...settings, transcriptionSource: "cloud" }} onToast={() => undefined} />);
  await userEvent.click(screen.getByRole("tab", { name: "Modele i API" }));
  const empty = await screen.findByRole("status");
  expect(empty).toHaveTextContent("Brak dostępnych dostawców.");
  expect(screen.queryByText("Sprawdzam…")).toBeNull();
  await userEvent.click(within(empty).getByRole("button", { name: "Ponów" }));
  await waitFor(() => expect(screen.getByRole("combobox", { name: "Dostawca" })).toBeEnabled());
  expect(screen.getByRole("combobox", { name: "Dostawca" })).toHaveTextContent("xAI (Grok)");
  expect(listCloudProviders).toHaveBeenCalledTimes(2);
});

test.each(["success", "error"] as const)("custom endpoint ownership ignores old %s and discovers the new endpoint", async (completion) => {
  let persisted: typeof settings = { ...settings, transcriptionSource: "cloud", cloudProvider: "custom", cloudBaseUrl: "https://endpoint-a.test/v1" };
  let resolveOld!: (models: string[]) => void;
  let rejectOld!: (error: Error) => void;
  const endpoints: string[] = [];
  const listCloudModels = vi.fn(() => {
    endpoints.push(persisted.cloudBaseUrl);
    if (endpoints.length === 1) return Promise.resolve(["cached-a-model"]);
    if (endpoints.length === 2) return new Promise<string[]>((resolve, reject) => { resolveOld = resolve; rejectOld = reject; });
    return Promise.resolve(["endpoint-b-model"]);
  });
  const updateSettings = vi.fn(async (next: typeof settings) => { persisted = next; return { settings: next, warning: null }; });
  const adapter = adapterStub({ getSettings: async () => persisted, updateSettings, listCloudModels, listCloudKeys: async () => ["custom"] });
  const onToast = vi.fn();
  renderWithI18n(<SettingsPage adapter={adapter} initialSettings={persisted} onToast={onToast} />);
  await userEvent.click(screen.getByRole("tab", { name: "Modele i API" }));
  const fetch = await screen.findByRole("button", { name: "Pobierz listę modeli" });
  await waitFor(() => expect(fetch).toBeEnabled());
  await userEvent.click(fetch);
  await waitFor(() => expect(onToast).toHaveBeenCalledWith("Pobrano 1 modeli.", "success"));
  onToast.mockClear();
  await userEvent.click(fetch);
  expect(screen.getByRole("button", { name: "Pobieram…" })).toBeDisabled();
  await userEvent.type(screen.getByLabelText("Klucz API"), "unsaved-local-key");
  const address = screen.getByLabelText("Adres API");
  await userEvent.clear(address);
  await userEvent.type(address, "https://endpoint-b.test/v1{Enter}");
  await waitFor(() => expect(updateSettings).toHaveBeenCalledWith(expect.objectContaining({ cloudBaseUrl: "https://endpoint-b.test/v1" })));
  await act(async () => {
    if (completion === "success") resolveOld(["late-a-model"]);
    else rejectOld(new Error("Endpoint A failed"));
  });
  expect(screen.getByLabelText("Klucz API")).toHaveValue("unsaved-local-key");
  expect(screen.queryByRole("alert")).toBeNull();
  expect(onToast).not.toHaveBeenCalled();
  await userEvent.click(screen.getByRole("combobox", { name: "Model" }));
  expect(screen.queryByRole("option", { name: "cached-a-model" })).toBeNull();
  expect(screen.queryByRole("option", { name: "late-a-model" })).toBeNull();
  await userEvent.keyboard("{Escape}");
  await userEvent.click(screen.getByRole("button", { name: "Pobierz listę modeli" }));
  await waitFor(() => expect(onToast).toHaveBeenCalledWith("Pobrano 1 modeli.", "success"));
  await userEvent.click(screen.getByRole("combobox", { name: "Model" }));
  expect(await screen.findByRole("option", { name: "endpoint-b-model" })).toBeVisible();
  expect(endpoints).toEqual(["https://endpoint-a.test/v1", "https://endpoint-a.test/v1", "https://endpoint-b.test/v1"]);
});
