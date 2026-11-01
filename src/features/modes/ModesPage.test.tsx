import { act, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";

import { ModesPage } from "./ModesPage";
import { adapterStub } from "../../test/fixtures";
import { settings } from "../../test/fixtures";
import { renderWithI18n } from "../../test/renderWithI18n";
import type { Mode } from "../../lib/types";

describe("tryby", () => {
  test("Użyj zapisuje dokładny nowy tryb przed jego aktywacją", async () => {
    const user = userEvent.setup();
    let finishSave!: () => void;
    const upsertMode = vi.fn<(mode: Mode) => Promise<void>>(() => new Promise<void>((resolve) => { finishSave = resolve; }));
    const updateSettings = vi.fn(async (next) => ({ settings: next, warning: null }));
    renderWithI18n(<ModesPage adapter={adapterStub({ upsertMode, updateSettings })} settings={settings} onSettingsChange={vi.fn()} onToast={vi.fn()} />);
    await user.click(await screen.findByRole("button", { name: "Nowy tryb" }));
    expect(screen.getByRole("button", { name: "Użyj" })).toBeDisabled();
    await user.type(screen.getByLabelText("Nazwa"), "Notatki");
    await user.type(screen.getByLabelText("Instrukcja"), "layout: bullets");
    expect(screen.getByRole("checkbox", { name: "Dostępny" })).toBeChecked();
    await user.click(screen.getByRole("button", { name: "Użyj" }));
    expect(upsertMode).toHaveBeenCalledWith(expect.objectContaining({ name: "Notatki", prompt: "layout: bullets", enabled: true }));
    expect(updateSettings).not.toHaveBeenCalled();
    await act(async () => finishSave());
    await waitFor(() => expect(updateSettings).toHaveBeenCalledWith(expect.objectContaining({ activeMode: upsertMode.mock.calls[0][0].id })));
    expect(await screen.findByRole("button", { name: /Notatki/ })).toBeVisible();
  });

  test("błąd zapisu przed Użyj zachowuje aktywny tryb i umożliwia ponowienie", async () => {
    const user = userEvent.setup();
    const updateSettings = vi.fn();
    const onToast = vi.fn();
    renderWithI18n(<ModesPage adapter={adapterStub({ upsertMode: async () => { throw new Error("Baza zajęta"); }, updateSettings })} settings={settings} onSettingsChange={vi.fn()} onToast={onToast} />);
    await user.click(await screen.findByRole("button", { name: "Nowy tryb" }));
    await user.type(screen.getByLabelText("Nazwa"), "Notatki");
    await user.click(screen.getByRole("button", { name: "Użyj" }));
    expect(updateSettings).not.toHaveBeenCalled();
    expect(onToast).toHaveBeenCalledWith(expect.stringContaining("Baza zajęta"), "error");
    expect(screen.getByRole("button", { name: "Użyj" })).toBeEnabled();
    expect(screen.getByLabelText("Nazwa")).toHaveValue("Notatki");
  });

  test("chroni wbudowany tryb i zapisuje własny", async () => {
    const user = userEvent.setup();
    renderWithI18n(<ModesPage adapter={adapterStub()} settings={settings} onSettingsChange={() => undefined} onToast={() => undefined} />);
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
    renderWithI18n(<ModesPage
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
    renderWithI18n(<ModesPage
      adapter={adapterStub({ listModes })}
      settings={settings}
      onSettingsChange={() => undefined}
      onToast={onToast}
    />);
    expect(await screen.findByText("Nie udało się wczytać trybów", { selector: "strong" })).toBeVisible();
    await userEvent.click(screen.getByRole("button", { name: "Spróbuj ponownie" }));
    expect(listModes).toHaveBeenCalledTimes(2);
  });

  test("nie pozwala użyć wyłączonego ani usunąć aktualnie używanego trybu", async () => {
    const active = {
      id: "active-custom", name: "Aktywny", description: "Aktywny", prompt: "prefix: A",
      enabled: true, isDefault: false, createdAt: 1,
    };
    const disabled = {
      id: "disabled-custom", name: "Wyłączony", description: "Wyłączony", prompt: "case: lower",
      enabled: false, isDefault: false, createdAt: 2,
    };
    renderWithI18n(
      <ModesPage
        adapter={adapterStub({ listModes: async () => [active, disabled] })}
        settings={{ ...settings, activeMode: active.id }}
        onSettingsChange={() => undefined}
        onToast={() => undefined}
      />,
    );

    await userEvent.click(await screen.findByRole("button", { name: /Aktywny/ }));
    expect(screen.getByRole("button", { name: "Usuń tryb" })).toBeDisabled();
    await userEvent.click(screen.getByRole("button", { name: /Wyłączony/ }));
    expect(screen.getByRole("button", { name: "Użyj" })).toBeDisabled();
  });

  test("opisuje wyłącznie obsługiwany lokalny DSL", async () => {
    renderWithI18n(<ModesPage adapter={adapterStub()} settings={settings} onSettingsChange={() => undefined} onToast={() => undefined} />);

    await screen.findByLabelText("Instrukcja");
    expect(screen.getByText(/case: lower\|upper\|sentence/i)).toBeVisible();
    expect(screen.getByLabelText("Instrukcja")).toHaveAttribute("placeholder", expect.stringContaining("layout: bullets"));
  });
});
