import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test } from "vitest";

import { App } from "./App";
import { adapterStub } from "../test/fixtures";

describe("główna nawigacja", () => {
  test("przechodzi klawiaturą między wszystkimi sekcjami", async () => {
    const user = userEvent.setup();
    render(<App adapter={adapterStub()} />);

    expect(await screen.findByRole("heading", { name: /dzień dobry/i })).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Historia" }));
    expect(await screen.findByRole("heading", { name: "Historia" })).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Słownik" }));
    expect(await screen.findByRole("heading", { name: "Słownik" })).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Tryby" }));
    expect(await screen.findByRole("heading", { name: "Tryby" })).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Ustawienia" }));
    expect(await screen.findByRole("heading", { name: "Ustawienia" })).toBeVisible();
  });
});
