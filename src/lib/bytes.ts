/**
 * Sizes as a person would say them: "670 MB", not "670478772".
 *
 * Decimal units, because that is what the model host quotes and what a
 * download of the same file will be reported as elsewhere.
 */
const UNITS = ["B", "KB", "MB", "GB", "TB"];

export function formatBytes(bytes: number | null | undefined, fallback = ""): string {
  if (!bytes) return fallback;
  let value = bytes;
  let unit = 0;
  while (value >= 1000 && unit < UNITS.length - 1) {
    value /= 1000;
    unit += 1;
  }
  // One decimal below ten, where the difference is worth reading.
  return `${value >= 10 || unit === 0 ? value.toFixed(0) : value.toFixed(1)} ${UNITS[unit]}`;
}
