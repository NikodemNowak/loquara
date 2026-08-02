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

  test("lokalizuje znane komunikaty backendu", () => {
    expect(normalizeError("Previous dictation was interrupted before audio finalization.")).toBe(
      "Poprzednie dyktowanie przerwano przed zapisaniem audio.",
    );
    expect(normalizeError("Stop dictating before deleting a model.")).toBe(
      "Zatrzymaj dyktowanie przed usunięciem modelu.",
    );
  });

  test("lokalizuje komunikaty backendu z dynamicznymi częściami", () => {
    expect(normalizeError("The selected model is not ready. Download it before retrying: parakeet.")).toBe(
      "Wybrany model nie jest gotowy. Pobierz go przed ponowieniem: parakeet.",
    );
    expect(normalizeError("Missing or empty files: config.json.")).toBe(
      "Brakujące lub puste pliki: config.json.",
    );
    expect(normalizeError('Mode "clean" is disabled.')).toBe("Tryb „clean” jest wyłączony.");
  });

  test("bezpiecznie obsługuje cykliczny obiekt", () => {
    const cyclic: Record<string, unknown> = {};
    cyclic.self = cyclic;

    expect(normalizeError(cyclic)).toBe("Nieznany błąd.");
  });
});
