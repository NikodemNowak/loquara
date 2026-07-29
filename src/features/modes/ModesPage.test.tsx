import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test } from "vitest";

import { ModesPage } from "./ModesPage";
import { adapterStub } from "../../test/fixtures";

describe("tryby", () => {
  test("chroni wbudowany tryb i zapisuje własny", async () => {
    const user = userEvent.setup();
    render(<ModesPage adapter={adapterStub()} />);
    await user.click(await screen.findByRole("button", { name: /Czysty/ }));
    expect(screen.getByRole("button", { name: "Usuń tryb" })).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "Nowy tryb" }));
    await user.type(screen.getByLabelText("Nazwa"), "Notatki");
    await user.type(screen.getByLabelText("Opis"), "Krótkie punkty");
    await user.type(screen.getByLabelText("Instrukcja"), "Zapisz jako punkty.");
    await user.click(screen.getByRole("button", { name: "Zapisz tryb" }));
    expect(await screen.findByRole("button", { name: /Notatki/ })).toBeVisible();
  });
});
