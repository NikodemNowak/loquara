import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";

import { VocabularyPage } from "./VocabularyPage";
import { adapterStub } from "../../test/fixtures";

test("dodaje, wyszukuje i usuwa wpis słownika", async () => {
  const user = userEvent.setup();
  render(<VocabularyPage adapter={adapterStub()} onToast={() => undefined} />);
  await user.type(screen.getByLabelText("Usłyszane"), "parakit");
  await user.type(screen.getByLabelText("Zamień na"), "Parakeet");
  await user.click(screen.getByRole("button", { name: "Dodaj zamianę" }));
  expect(await screen.findByText("parakit")).toBeVisible();
  await user.type(screen.getByRole("searchbox"), "para");
  await user.click(screen.getByRole("button", { name: "Usuń parakit" }));
  expect(screen.queryByText("parakit")).not.toBeInTheDocument();
});

describe("błędy słownika", () => {
  test("pokazuje błąd wczytywania i pozwala ponowić", async () => {
    const listVocabulary = vi.fn()
      .mockRejectedValueOnce(new Error("Baza zajęta"))
      .mockResolvedValueOnce([{ id: 1, heard: "parakit", replacement: "Parakeet" }]);
    render(<VocabularyPage adapter={adapterStub({ listVocabulary })} onToast={() => undefined} />);

    expect(await screen.findByText("Nie udało się wczytać słownika")).toBeVisible();
    await userEvent.click(screen.getByRole("button", { name: "Spróbuj ponownie" }));
    expect(await screen.findByText("parakit")).toBeVisible();
  });

  test("pokazuje błąd zapisu i zachowuje formularz", async () => {
    const onToast = vi.fn();
    render(
      <VocabularyPage
        adapter={adapterStub({ addVocabulary: vi.fn(async () => { throw new Error("Brak dostępu"); }) })}
        onToast={onToast}
      />,
    );
    await screen.findByText("Zapisane zamiany");
    await userEvent.type(screen.getByLabelText("Usłyszane"), "parakit");
    await userEvent.type(screen.getByLabelText("Zamień na"), "Parakeet");
    await userEvent.click(screen.getByRole("button", { name: "Dodaj zamianę" }));

    expect(onToast).toHaveBeenCalledWith(expect.stringContaining("Brak dostępu"), "error");
    expect(screen.getByLabelText("Usłyszane")).toHaveValue("parakit");
    expect(screen.getByRole("button", { name: "Dodaj zamianę" })).toBeEnabled();
  });
});
