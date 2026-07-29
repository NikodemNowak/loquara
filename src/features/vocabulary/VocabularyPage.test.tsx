import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test } from "vitest";

import { VocabularyPage } from "./VocabularyPage";
import { adapterStub } from "../../test/fixtures";

test("dodaje, wyszukuje i usuwa wpis słownika", async () => {
  const user = userEvent.setup();
  render(<VocabularyPage adapter={adapterStub()} />);
  await user.type(screen.getByLabelText("Usłyszane"), "parakit");
  await user.type(screen.getByLabelText("Zamień na"), "Parakeet");
  await user.click(screen.getByRole("button", { name: "Dodaj zamianę" }));
  expect(await screen.findByText("parakit")).toBeVisible();
  await user.type(screen.getByRole("searchbox"), "para");
  await user.click(screen.getByRole("button", { name: "Usuń parakit" }));
  expect(screen.queryByText("parakit")).not.toBeInTheDocument();
});
