import "@testing-library/jest-dom/vitest";

import { afterEach } from "vitest";
import { cleanup } from "@testing-library/react";

afterEach(cleanup);

/* jsdom implements neither pointer capture nor scrollIntoView, both of which
 * the dropdown calls while opening. Without them it throws instead of showing
 * its list, so the tests below would exercise nothing. */
if (typeof Element !== "undefined") {
  Element.prototype.hasPointerCapture ??= () => false;
  Element.prototype.setPointerCapture ??= () => undefined;
  Element.prototype.releasePointerCapture ??= () => undefined;
  Element.prototype.scrollIntoView ??= () => undefined;
}
