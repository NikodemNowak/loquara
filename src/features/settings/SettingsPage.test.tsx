import { render, screen } from "@testing-library/react";
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
});
