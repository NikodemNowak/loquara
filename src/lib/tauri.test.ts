import { describe, expect, test } from "vitest";

import { createBrowserAdapter, platformErrorMessage } from "./tauri";

describe("adapter demonstracyjny", () => {
  test("izoluje dane każdej instancji i nie zwraca danych po angielsku", async () => {
    const first = createBrowserAdapter();
    const second = createBrowserAdapter();
    const before = await second.listVocabulary();

    await first.addVocabulary("parakit", "Parakeet");

    expect(await second.listVocabulary()).toEqual(before);
    const history = await first.listHistory({});
    expect(history.length).toBeGreaterThan(2);
    expect(history.some((item) => item.text?.includes("spotkania"))).toBe(true);
  });

  test("normalizuje obiektowy błąd platformy i zachowuje zgodność ze stringiem", () => {
    expect(platformErrorMessage({ code: "focus_failed", message: "Nie udało się przywrócić okna." }))
      .toBe("Nie udało się przywrócić okna.");
    expect(platformErrorMessage("Starszy błąd")).toBe("Starszy błąd");
    expect(platformErrorMessage({ error: { message: "Błąd schowka" } })).toBe("Błąd schowka");
    expect(platformErrorMessage({ code: "unknown" })).toBe("{\"code\":\"unknown\"}");
  });

  test("demo raportuje prawdziwy kontrakt gotowego modelu", async () => {
    expect(await createBrowserAdapter().getModelStatus()).toMatchObject({
      state: "ready",
      model: "nvidia/parakeet-tdt-0.6b-v3",
    });
  });
});
