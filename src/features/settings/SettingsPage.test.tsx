import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";

import { SettingsPage } from "./SettingsPage";
import { adapterStub, settings } from "../../test/fixtures";

describe("ustawienia", () => {
  test("stosuje wybrany motyw na dokumencie", async () => {
    render(<SettingsPage adapter={adapterStub()} initialSettings={settings} onToast={() => undefined} />);
    await userEvent.click(screen.getByRole("radio", { name: "Ciemny" }));
    expect(document.documentElement).toHaveAttribute("data-theme", "dark");
  });

  test("cofa nieudaną zmianę i pokazuje komunikat", async () => {
    const onToast = vi.fn();
    const updateSettings = vi.fn(async () => { throw new Error("Brak uprawnień"); });
    render(<SettingsPage adapter={adapterStub({ updateSettings })} initialSettings={settings} onToast={onToast} />);
    const toggle = screen.getByRole("checkbox", { name: "Wklejaj automatycznie" });
    await userEvent.click(toggle);
    expect(toggle).toBeChecked();
    expect(onToast).toHaveBeenCalledWith(expect.stringContaining("Brak uprawnień"), "error");
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
    const first = render(
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
    render(
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
    render(<SettingsPage
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

  test("pokazuje błąd listy mikrofonów i pozwala ponowić", async () => {
    const listInputDevices = vi.fn()
      .mockRejectedValueOnce(new Error("Brak dostępu"))
      .mockResolvedValueOnce([]);
    render(<SettingsPage
      adapter={adapterStub({ listInputDevices })}
      initialSettings={settings}
      onSettingsChange={() => undefined}
      onToast={() => undefined}
    />);
    expect(await screen.findByText(/Nie udało się wczytać mikrofonów/)).toBeVisible();
    await userEvent.click(screen.getByRole("button", { name: "Spróbuj ponownie" }));
    await waitFor(() => expect(listInputDevices).toHaveBeenCalledTimes(2));
  });
});
