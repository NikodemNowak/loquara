import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";

import { ModesPage } from "./ModesPage";
import { adapterStub } from "../../test/fixtures";
import { settings } from "../../test/fixtures";

describe("tryby", () => {
  test("chroni wbudowany tryb i zapisuje własny", async () => {
    const user = userEvent.setup();
    render(<ModesPage adapter={adapterStub()} settings={settings} onSettingsChange={() => undefined} onToast={() => undefined} />);
    await user.click(await screen.findByRole("button", { name: /Czysty/ }));
    expect(screen.getByRole("button", { name: "Usuń tryb" })).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "Nowy tryb" }));
    await user.type(screen.getByLabelText("Nazwa"), "Notatki");
    await user.type(screen.getByLabelText("Opis"), "Krótkie punkty");
    await user.type(screen.getByLabelText("Instrukcja"), "Zapisz jako punkty.");
    await user.click(screen.getByRole("button", { name: "Zapisz tryb" }));
    expect(await screen.findByRole("button", { name: /Notatki/ })).toBeVisible();
  });

  test("rozpoznaje wbudowane ID niezależnie od isDefault i przełącza aktywny tryb", async () => {
    const updateSettings = vi.fn(async (next) => ({ settings: next, warning: null }));
    const code = {
      id: "code", name: "Kod", description: "Kod", prompt: "Kod", enabled: true,
      isDefault: false, createdAt: 1,
    };
    const custom = {
      id: "custom", name: "Własny", description: "Własny", prompt: "Własny", enabled: true,
      isDefault: true, createdAt: 2,
    };
    const onSettingsChange = vi.fn();
    render(<ModesPage
      adapter={adapterStub({
        listModes: async () => [code, custom],
        updateSettings,
        getSettings: async () => ({ ...settings, activeMode: "code" }),
      })}
      settings={{ ...settings, activeMode: "clean" }}
      onSettingsChange={onSettingsChange}
      onToast={() => undefined}
    />);
    await userEvent.click(await screen.findByRole("button", { name: /Kod/ }));
    expect(screen.getByRole("button", { name: "Usuń tryb" })).toBeDisabled();
    await userEvent.click(screen.getByRole("button", { name: "Użyj" }));
    expect(updateSettings).toHaveBeenCalledWith(expect.objectContaining({ activeMode: "code" }));
    expect(onSettingsChange).toHaveBeenCalledWith(expect.objectContaining({ activeMode: "code" }));
    await userEvent.click(screen.getByRole("button", { name: /Własny/ }));
    expect(screen.getByRole("button", { name: "Usuń tryb" })).toBeEnabled();
  });

  test("pokazuje błąd ładowania i zachowuje tryb po błędzie akcji", async () => {
    const listModes = vi.fn()
      .mockRejectedValueOnce(new Error("Baza zajęta"))
      .mockResolvedValueOnce([]);
    const onToast = vi.fn();
    render(<ModesPage
      adapter={adapterStub({ listModes })}
      settings={settings}
      onSettingsChange={() => undefined}
      onToast={onToast}
    />);
    expect(await screen.findByText("Nie udało się wczytać trybów", { selector: "strong" })).toBeVisible();
    await userEvent.click(screen.getByRole("button", { name: "Spróbuj ponownie" }));
    expect(listModes).toHaveBeenCalledTimes(2);
  });
});
