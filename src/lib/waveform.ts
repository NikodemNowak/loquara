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

/** Parses `#rrggbb` into channels; anything else comes back as null. */
function channels(colour: string): [number, number, number] | null {
  const hex = colour.trim();
  if (!/^#[0-9a-f]{6}$/i.test(hex)) return null;
  return [
    parseInt(hex.slice(1, 3), 16),
    parseInt(hex.slice(3, 5), 16),
    parseInt(hex.slice(5, 7), 16),
  ];
}

/**
 * Blends two colours, `t` of the way from the first to the second.
 *
 * Used to move a meter between "the room" and "you", which is a gradual
 * thing: switching colours outright would strobe on every breath.
 */
export function mixColour(from: string, to: string, t: number): string {
  const a = channels(from);
  const b = channels(to);
  if (!a || !b) return t > 0.5 ? to : from;
  const ratio = Math.max(0, Math.min(1, t));
  const channel = (index: number) => Math.round(a[index] + (b[index] - a[index]) * ratio);
  return `rgb(${channel(0)}, ${channel(1)}, ${channel(2)})`;
}

/**
 * How fast the room's own level is learned.
 *
 * A microphone in a normal room is never at zero: fans, traffic, the machine
 * itself. Metering raw loudness therefore shows a meter that is always half
 * up and cannot tell being spoken to from being switched on. The quietest
 * level lately is taken as the room, and only what rises above it counts.
 * It drops to a new quiet quickly and creeps up slowly, so a pause between
 * sentences is not mistaken for a quieter room.
 */
const FLOOR_FALL = 0.08;
const FLOOR_RISE = 0.0015;
/** How far above the room a level has to sit before it reads as a voice. */
const VOICE_MARGIN = 1.6;

/** The room's level after hearing `level`. */
export function trackRoom(level: number, floor: number): number {
  const heard = Math.max(0, level);
  return floor + (heard - floor) * (heard < floor ? FLOOR_FALL : FLOOR_RISE);
}

/** How far `level` stands out from the room; zero when it does not. */
export function aboveRoom(level: number, floor: number): number {
  return Math.max(0, Math.max(0, level) - floor * VOICE_MARGIN);
}
