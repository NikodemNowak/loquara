import { describe, expect, test } from "vitest";

import { createBrowserAdapter } from "./tauri";

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
});
