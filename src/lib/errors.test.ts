import { describe, expect, test } from "vitest";

import { normalizeError } from "./errors";

describe("normalizeError", () => {
  test.each([
    ["Błąd tekstowy", "Błąd tekstowy"],
    [new Error("Błąd Error"), "Błąd Error"],
    [{ message: "Błąd obiektu" }, "Błąd obiektu"],
    [{ error: { message: "Błąd zagnieżdżony" } }, "Błąd zagnieżdżony"],
    [{ code: "busy", retryable: true }, "{\"code\":\"busy\",\"retryable\":true}"],
  ])("normalizuje %o", (input, expected) => {
    expect(normalizeError(input)).toBe(expected);
    expect(normalizeError(input)).not.toContain("[object Object]");
  });

  test("bezpiecznie obsługuje cykliczny obiekt", () => {
    const cyclic: Record<string, unknown> = {};
    cyclic.self = cyclic;

    expect(normalizeError(cyclic)).toBe("Nieznany błąd.");
  });
});
