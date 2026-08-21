/** Shared waveform rendering.
 *
 * The waveform is Loquara's one recurring visual: live in the recorder pill,
 * static in the history list, scrubbable in the player. All three draw through
 * this module so a recording looks like the same recording everywhere.
 */

/** Envelope bytes as stored by the recorder: one bucket, `0..=255`. */
export type Peaks = readonly number[];

export interface WaveformStyle {
  /** Colour for buckets left of `progress`. */
  played: string;
  /** Colour for buckets right of `progress`. */
  pending: string;
  /** Bar width in CSS pixels. */
  barWidth?: number;
  /** Gap between bars in CSS pixels. */
  gap?: number;
  /** Fraction of the envelope already played, `0..=1`. Defaults to all pending. */
  progress?: number;
}

const DEFAULT_BAR = 2;
const DEFAULT_GAP = 1.5;
/** Keeps silence visible as a hairline rather than vanishing entirely. */
const MIN_BAR_HEIGHT = 2;

/**
 * Sizes a canvas to its CSS box at native device resolution.
 *
 * Returns the CSS-pixel dimensions to draw against, or null when the canvas is
 * not laid out yet (zero width) or 2D rendering is unavailable — in jsdom, for
 * instance, where `getContext` returns null.
 */
export function prepareCanvas(
  canvas: HTMLCanvasElement,
  cssHeight: number,
): { context: CanvasRenderingContext2D; width: number; height: number } | null {
  const width = canvas.clientWidth;
  if (width <= 0) return null;
  const context = canvas.getContext("2d");
  if (!context) return null;
  const dpr = window.devicePixelRatio || 1;
  canvas.width = Math.round(width * dpr);
  canvas.height = Math.round(cssHeight * dpr);
  context.setTransform(dpr, 0, 0, dpr, 0, 0);
  return { context, width, height: cssHeight };
}

/**
 * Resamples an envelope to exactly `count` bars.
 *
 * Bars take the maximum of the buckets they cover, matching how the recorder
 * downsamples: a quiet mean would hide the syllables that make speech legible.
 */
export function resamplePeaks(peaks: Peaks, count: number): number[] {
  if (count <= 0) return [];
  if (peaks.length === 0) return new Array<number>(count).fill(0);
  return Array.from({ length: count }, (_, index) => {
    const start = Math.floor((index * peaks.length) / count);
    const end = Math.max(start + 1, Math.floor(((index + 1) * peaks.length) / count));
    let loudest = 0;
    for (let cursor = start; cursor < Math.min(end, peaks.length); cursor += 1) {
      const value = peaks[cursor];
      if (Number.isFinite(value) && value > loudest) loudest = value;
    }
    return loudest;
  });
}

/**
 * Draws a stored envelope as a centred bar waveform.
 *
 * Bars are laid out to fill the width exactly, so the same recording keeps its
 * shape whether it is drawn 56px wide in a list or 600px wide in the player.
 */
export function drawPeaks(
  context: CanvasRenderingContext2D,
  peaks: Peaks,
  width: number,
  height: number,
  style: WaveformStyle,
): void {
  const barWidth = style.barWidth ?? DEFAULT_BAR;
  const gap = style.gap ?? DEFAULT_GAP;
  const step = barWidth + gap;
  const count = Math.max(1, Math.floor((width + gap) / step));
  const bars = resamplePeaks(peaks, count);
  const boundary = (style.progress ?? 0) * count;

  context.clearRect(0, 0, width, height);
  for (let index = 0; index < count; index += 1) {
    const amplitude = Math.min(1, Math.max(0, bars[index] / 255));
    const barHeight = Math.max(MIN_BAR_HEIGHT, amplitude * height);
    context.fillStyle = index < boundary ? style.played : style.pending;
    context.fillRect(
      index * step,
      (height - barHeight) / 2,
      barWidth,
      barHeight,
    );
  }
}

/**
 * Reads a CSS custom property off the document root.
 *
 * Canvas cannot reference CSS variables, so colours are resolved at draw time
 * to keep the waveform tied to the token system rather than to literals.
 */
export function cssColor(name: string, fallback: string): string {
  const value = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return value || fallback;
}
