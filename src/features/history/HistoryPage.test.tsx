import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";

import { HistoryPage } from "./HistoryPage";
import { adapterStub, recordings, snapshot } from "../../test/fixtures";

import { renderWithI18n } from "../../test/renderWithI18n";

function renderPage(adapter = adapterStub(), onToast = () => undefined) {
  return renderWithI18n(<HistoryPage adapter={adapter} recordings={recordings} onRefresh={async () => undefined} onToast={onToast} />);
}

describe("historia", () => {
  test("wyszukuje, wybiera i uruchamia akcje dla nagrania", async () => {
    const user = userEvent.setup();
    const pasteTranscript = vi.fn(async () => undefined);
    const retryTranscription = vi.fn(async () => snapshot);
    const adapter = adapterStub({ pasteTranscript, retryTranscription });
    renderPage(adapter);

    await user.type(screen.getByRole("searchbox"), "podsumowanie");
    expect(screen.getByRole("button", { name: /Przygotuj proszę/ })).toBeVisible();
    expect(screen.queryByText("Model chwilowo niedostępny")).not.toBeInTheDocument();

    await user.clear(screen.getByRole("searchbox"));
    await user.click(screen.getByRole("button", { name: /Model chwilowo niedostępny/ }));
    const inspector = screen.getByRole("complementary", { name: "Szczegóły nagrania" });
    await user.click(within(inspector).getByRole("button", { name: "Ponów" }));
    expect(retryTranscription).toHaveBeenCalledWith("failed-1");

    await user.click(screen.getByRole("button", { name: /Przygotuj proszę/ }));
    await user.click(within(inspector).getByRole("button", { name: "Wklej" }));
    expect(pasteTranscript).toHaveBeenCalledWith("complete-1");
  });

  test("blokuje usuwanie aktywnego nagrania", async () => {
    renderPage();
    await userEvent.click(screen.getByRole("button", { name: /Aktywne nagranie/ }));
    expect(within(screen.getByRole("complementary")).getByRole("button", { name: "Usuń" })).toBeDisabled();
  });

  test("pokazuje błąd akcji i zachowuje wybrane nagranie", async () => {
    const onToast = vi.fn();
    const adapter = adapterStub({
      pasteTranscript: vi.fn(async () => { throw new Error("Schowek niedostępny"); }),
    });
    renderPage(adapter, onToast);

    await userEvent.click(screen.getByRole("button", { name: /Przygotuj proszę/ }));
    const inspector = screen.getByRole("complementary", { name: "Szczegóły nagrania" });
    await userEvent.click(within(inspector).getByRole("button", { name: "Wklej" }));

    expect(onToast).toHaveBeenCalledWith(expect.stringContaining("Schowek niedostępny"), "error");
    expect(within(inspector).getByText("Przygotuj proszę podsumowanie spotkania.")).toBeVisible();
    expect(within(inspector).getByRole("button", { name: "Wklej" })).toBeEnabled();
  });

  test("chroni kopiowanie, pokazuje stan pracy i normalizuje odrzucenie schowka", async () => {
    let rejectCopy!: (reason: unknown) => void;
    const writeText = vi.fn(() => new Promise<void>((_, reject) => { rejectCopy = reject; }));
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    const onToast = vi.fn();
    renderPage(adapterStub(), onToast);

    await userEvent.click(screen.getByRole("button", { name: /Przygotuj proszę/ }));
    await userEvent.click(screen.getByRole("button", { name: "Kopiuj" }));
    expect(screen.getByRole("button", { name: "Kopiuję…" })).toBeDisabled();
    rejectCopy({ error: { message: "Brak dostępu do schowka" } });

    expect(await screen.findByRole("button", { name: "Kopiuj" })).toBeEnabled();
    expect(onToast).toHaveBeenCalledWith(expect.stringContaining("Brak dostępu do schowka"), "error");
    expect(writeText).toHaveBeenCalledWith("Przygotuj proszę podsumowanie spotkania.");
  });
});
