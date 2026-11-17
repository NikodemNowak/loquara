import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";

import { VocabularyPage } from "./VocabularyPage";
import { adapterStub } from "../../test/fixtures";
import { renderWithI18n } from "../../test/renderWithI18n";

test("dodaje słowo z wariantami, wyszukuje i usuwa", async () => {
  const user = userEvent.setup();
  const adapter = adapterStub();
  const addVocabulary = vi.spyOn(adapter, "addVocabulary");
  renderWithI18n(<VocabularyPage adapter={adapter} onToast={() => undefined} />);
  await screen.findByText("0 słów");
  expect(screen.queryByLabelText("Inne zapisy (po przecinku)")).toBeNull();
  await user.type(screen.getByLabelText("Słowo lub fraza"), "Claude Code");
  await user.click(screen.getByRole("button", { name: "Słyszane też jako" }));
  await user.type(screen.getByLabelText("Inne zapisy (po przecinku)"), " klod kod, ,klaud kod {Enter}");

  expect(addVocabulary).toHaveBeenCalledWith("Claude Code", ["klod kod", "klaud kod"]);
  expect(await screen.findByText("Claude Code")).toBeVisible();
  expect(screen.getByText("też: klod kod, klaud kod")).toBeVisible();
  expect(screen.getByLabelText("Słowo lub fraza")).toHaveValue("");
  expect(screen.getByText("1 słowo")).toBeVisible();

  await user.type(screen.getByRole("searchbox"), "klaud");
  expect(screen.getByText("Claude Code")).toBeVisible();
  await user.click(screen.getByRole("button", { name: "Usuń Claude Code" }));
  expect(screen.queryByText("Claude Code")).not.toBeInTheDocument();
});

test("podpowiedź dodaje wariant do istniejącego słowa i znika", async () => {
  const user = userEvent.setup();
  const addVocabulary = vi.fn(async () => ({ id: 1, term: "Claude", aliases: ["kloda", "klod"] }));
  renderWithI18n(<VocabularyPage
    adapter={adapterStub({
      listVocabulary: async () => [{ id: 1, term: "Claude", aliases: ["kloda"] }],
      vocabularySuggestions: async () => [{ termId: 1, term: "Claude", heard: "klod", count: 4, example: "zapytaj klod o to" }],
      addVocabulary,
    })}
    onToast={() => undefined}
  />);

  expect(await screen.findByRole("heading", { name: "Podpowiedzi" })).toBeVisible();
  expect(screen.getByText("zapytaj klod o to")).toBeVisible();
  expect(screen.getByText("4×")).toBeVisible();
  await user.click(screen.getByRole("button", { name: "Dodaj „klod” jako wariant Claude" }));

  expect(addVocabulary).toHaveBeenCalledWith("Claude", ["klod"]);
  expect(await screen.findByText("też: kloda, klod")).toBeVisible();
  expect(screen.getAllByText("Claude")).toHaveLength(1);
  expect(screen.queryByRole("heading", { name: "Podpowiedzi" })).toBeNull();
});

test("ukrywa podpowiedzi, gdy ich wczytanie się nie powiedzie", async () => {
  renderWithI18n(<VocabularyPage
    adapter={adapterStub({
      listVocabulary: async () => [{ id: 1, term: "Loquara", aliases: [] }],
      vocabularySuggestions: async () => { throw new Error("Brak historii"); },
    })}
    onToast={() => undefined}
  />);
  const row = (await screen.findByText("Loquara")).closest(".vocabulary-row") as HTMLElement;
  expect(within(row).queryByText(/też:/)).toBeNull();
  expect(screen.queryByRole("heading", { name: "Podpowiedzi" })).toBeNull();
});

describe("błędy słownika", () => {
  test("pokazuje błąd wczytywania i pozwala ponowić", async () => {
    const listVocabulary = vi.fn()
      .mockRejectedValueOnce(new Error("Baza zajęta"))
      .mockResolvedValueOnce([{ id: 1, term: "Parakeet", aliases: ["parakit"] }]);
    renderWithI18n(<VocabularyPage adapter={adapterStub({ listVocabulary })} onToast={() => undefined} />);

    expect(await screen.findByText("Nie udało się wczytać słownika")).toBeVisible();
    await userEvent.click(screen.getByRole("button", { name: "Spróbuj ponownie" }));
    expect(await screen.findByText("Parakeet")).toBeVisible();
    expect(screen.getByText("też: parakit")).toBeVisible();
  });

  test("pokazuje błąd zapisu i zachowuje formularz", async () => {
    const onToast = vi.fn();
    renderWithI18n(
      <VocabularyPage
        adapter={adapterStub({ addVocabulary: vi.fn(async () => { throw new Error("Brak dostępu"); }) })}
        onToast={onToast}
      />,
    );
    await screen.findByText("Twoje słowa");
    await userEvent.type(screen.getByLabelText("Słowo lub fraza"), "Parakeet");
    await userEvent.click(screen.getByRole("button", { name: "Dodaj" }));

    expect(onToast).toHaveBeenCalledWith(expect.stringContaining("Brak dostępu"), "error");
    expect(screen.getByLabelText("Słowo lub fraza")).toHaveValue("Parakeet");
    expect(screen.getByRole("button", { name: "Dodaj" })).toBeEnabled();
  });
});
