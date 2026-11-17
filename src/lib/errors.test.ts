import { describe, expect, test } from "vitest";

import { errorDetail, normalizeError } from "./errors";

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
    expect(normalizeError("Previous dictation was interrupted before completion.")).toBe(
      "Poprzednie dyktowanie przerwano zanim skończyło się przepisywanie.",
    );
    expect(normalizeError("audio input buffer overflowed")).toBe(
      "Kolejka mikrofonu się przepełniła. Nagranie mogło stracić fragmenty — spróbuj ponownie.",
    );
    expect(normalizeError("recording cannot be retried: recording audio is missing")).toBe(
      "Brak pliku nagrania — nie można ponowić transkrypcji.",
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
  });

  test.each([
    ["could not restore target focus: target unavailable; final text is in the clipboard", "Nie udało się wkleić — okno docelowe jest niedostępne. Tekst jest w schowku."],
    [{ code: "focus_failed", message: "target unavailable; final text is in the clipboard" }, "Nie udało się wkleić — okno docelowe jest niedostępne. Tekst jest w schowku."],
    ["paste injection failed: modifier keys are still held; final text is in the clipboard", "Nie wklejono, bo klawisze Ctrl, Alt lub Shift były wciąż wciśnięte. Tekst jest w schowku."],
    ["paste injection failed: input desktop unavailable; final text is in the clipboard", "Nie wklejono — pulpit jest zablokowany lub niedostępny. Tekst jest w schowku."],
    ["paste injection failed: delivery cancelled; final text is in the clipboard", "Wklejanie anulowano. Tekst jest w schowku."],
    ["could not restore target focus: SetForegroundWindow failed", "Nie udało się wrócić do okna docelowego. Tekst jest w schowku."],
    ["could not load the model: onnxruntime error 7", "Nie udało się wczytać modelu. Spróbuj ponownie albo pobierz model jeszcze raz w Ustawieniach."],
    ["recording is not active", "Nagrywanie nie jest aktywne."],
    ["audio error: recording is not active", "Nagrywanie nie jest aktywne."],
    ["the requested operation is invalid for the current state", "Tej czynności nie można wykonać w tej chwili."],
    ["another lifecycle operation is in progress", "Trwa inna operacja. Spróbuj ponownie za chwilę."],
    ["recording not found", "Nie znaleziono tego nagrania."],
  ])("lokalizuje błąd backendu %o", (input, expected) => {
    expect(normalizeError(input)).toBe(expected);
  });

  test("zachowuje surowy szczegół tylko dla przetłumaczonych komunikatów", () => {
    expect(errorDetail("could not load the model: onnxruntime error 7")).toBe("could not load the model: onnxruntime error 7");
    expect(errorDetail("Coś nieprzewidzianego")).toBeUndefined();
  });

  test("bezpiecznie obsługuje cykliczny obiekt", () => {
    const cyclic: Record<string, unknown> = {};
    cyclic.self = cyclic;

    expect(normalizeError(cyclic)).toBe("Nieznany błąd.");
  });
});
