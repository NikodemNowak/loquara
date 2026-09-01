import { describe, expect, test } from "vitest";

import { aboveRoom, mixColour, resampleEnvelope, trackRoom } from "./waveform";

/** Feeds a level in for `frames` and returns where the room settles. */
function settle(level: number, frames: number, floor = 0) {
  let current = floor;
  for (let i = 0; i < frames; i += 1) current = trackRoom(level, current);
  return current;
}

describe("odróżnianie głosu od pokoju", () => {
  test("stały szum tła przestaje się liczyć jako sygnał", () => {
    // Wentylator, ulica, sam komputer. Miernik na surowej głośności stoi
    // przez to w połowie wysokości i nie odróżnia „słucham" od „włączony".
    const noise = 0.06;

    const floor = settle(noise, 4000);

    expect(floor).toBeGreaterThan(noise * 0.5);
    expect(aboveRoom(noise, floor)).toBe(0);
  });

  test("głos ponad tłem wciąż jest sygnałem", () => {
    const floor = settle(0.06, 4000);

    // Mowa to nie jest ciche przekroczenie progu — to wielokrotność tła.
    expect(aboveRoom(0.35, floor)).toBeGreaterThan(0.15);
  });

  test("cisza po hałasie schodzi szybciej, niż tło rośnie", () => {
    const loud = settle(0.4, 200);
    const afterQuiet = settle(0.02, 60, loud);
    const afterLoudAgain = settle(0.4, 60, 0.02);

    expect(loud - afterQuiet).toBeGreaterThan(afterLoudAgain - 0.02);
  });
});

describe("mieszanie kolorów", () => {
  test("krańce dają dokładnie te kolory, które dostało", () => {
    expect(mixColour("#000000", "#ffffff", 0)).toBe("rgb(0, 0, 0)");
    expect(mixColour("#000000", "#ffffff", 1)).toBe("rgb(255, 255, 255)");
    expect(mixColour("#000000", "#ffffff", 0.5)).toBe("rgb(128, 128, 128)");
  });

  test("wartości spoza zakresu nie wypadają poza kolor", () => {
    expect(mixColour("#000000", "#ffffff", -3)).toBe("rgb(0, 0, 0)");
    expect(mixColour("#000000", "#ffffff", 9)).toBe("rgb(255, 255, 255)");
  });

  test("czego nie da się rozłożyć na kanały, nie jest zgadywane", () => {
    // Token motywu może być czymkolwiek, co rozumie CSS.
    expect(mixColour("rgb(1 2 3)", "#ffffff", 0.9)).toBe("#ffffff");
  });
});

describe("obwiednia mowy", () => {
  test("zagęszcza historię głośności do słupków, biorąc szczyt sylaby", () => {
    const history = [0.1, 0.8, 0.2, 0.9, 0.15, 0.4];
    expect(resampleEnvelope(history, 3)).toEqual([0.8, 0.9, 0.4]);
  });

  test("pusta historia zostaje ciszą, a nie zgadywanką", () => {
    expect(resampleEnvelope([], 4)).toEqual([0, 0, 0, 0]);
  });
});
