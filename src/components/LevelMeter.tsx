import { useEffect, useRef, useState } from "react";

import { cssColor, prepareCanvas } from "../lib/waveform";

/** Loudness is perceived closer to a power curve than to raw amplitude. */
const LOUDNESS_EXPONENT = 0.45;
/**
 * Input gain before the curve.
 *
 * Normal speech measured as normalised RMS sits around 0.2-0.4, nowhere near
 * 1.0, so an ungained meter never reaches even half height and reads as if it
 * is barely hearing you. This maps ordinary talking onto the top of the range.
 */
const GAIN = 3.2;
/** Attack is near-instant so speech registers; release trails so it reads. */
const ATTACK = 0.5;
const RELEASE = 0.12;
/** Bars never fully collapse, so silence still looks like it is listening
 *  rather than like a row of dots. */
const IDLE_LEVEL = 0.2;
/** How much shorter the outermost bars are than the middle ones. */
const CENTER_DROP = 0.28;
/** Floor of the per-bar shimmer, so bars stay tall instead of flickering low. */
const SHIMMER_FLOOR = 0.8;

/**
 * Fixed per-bar phases and speeds.
 *
 * Deterministic rather than random so the meter looks the same every launch,
 * and irregular enough that neighbouring bars never move in lockstep — which
 * is what makes a level meter look mechanical.
 */
const BARS = Array.from({ length: 64 }, (_, index) => {
  const noise = Math.sin(index * 127.1 + 311.7) * 43758.5453;
  return {
    phase: (noise - Math.floor(noise)) * Math.PI * 2,
    speed: 3.2 + (index % 5) * 0.55,
  };
});

export interface LevelMeterProps {
  /** Current microphone level, `0..=1`. */
  level: number;
  /** `thinking` shows activity with no microphone signal behind it.
   *  `countdown` keeps metering but puts the bars out from the right as a
   *  deadline approaches. */
  mode?: "live" | "thinking" | "countdown";
  /** `countdown` only: epoch ms the last bar goes out at. */
  deadline?: number;
  /** `countdown` only: how long the whole row stands for, in ms. */
  span?: number;
  height: number;
  barWidth?: number;
  gap?: number;
  className?: string;
  label?: string;
  /** CSS custom property holding the bar colour. */
  colorToken?: string;
}

/**
 * A microphone level meter: fixed bars that rise and fall in place.
 *
 * This is a mood meter, not an oscilloscope. How much the bars move tracks how
 * loudly you are speaking, but individual heights are shaped to look good
 * rather than to reproduce the signal. The only promise it makes is that
 * silence looks still and speech looks alive — and that part is honest.
 *
 * Contrast with `Waveform`, which draws a recording's real stored envelope.
 */
export function LevelMeter({
  level,
  mode = "live",
  deadline = 0,
  span = 1,
  height,
  barWidth = 3,
  gap = 3,
  className = "",
  label,
  colorToken = "--accent",
}: LevelMeterProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const levelRef = useRef(level);
  levelRef.current = level;
  // Read inside the animation loop rather than through the effect, so time
  // passing never costs a React render.
  const deadlineRef = useRef(deadline);
  deadlineRef.current = deadline;
  const spanRef = useRef(span);
  spanRef.current = span;
  // The window is resizable, so bar layout has to be recomputed when the
  // canvas box changes or the drawing ends up stretched.
  const [width, setWidth] = useState(0);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(([entry]) => {
      setWidth(Math.round(entry.contentRect.width));
    });
    observer.observe(canvas);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const prepared = prepareCanvas(canvas, height);
    if (!prepared) return;
    const { context, width } = prepared;
    const step = barWidth + gap;
    const count = Math.min(BARS.length, Math.max(3, Math.floor((width + gap) / step)));
    const contentWidth = count * step - gap;
    const offset = Math.max(0, (width - contentWidth) / 2);
    const middle = (count - 1) / 2;
    const heights = new Array<number>(count).fill(IDLE_LEVEL);
    const reduceMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    let energy = 0;
    let raf = 0;

    const draw = (now: number) => {
      const time = reduceMotion ? 0 : now / 1000;
      const shaped = mode === "thinking"
        ? 0.6
        : Math.min(1, Math.pow(Math.max(0, levelRef.current) * GAIN, LOUDNESS_EXPONENT));
      energy += (shaped - energy) * (shaped > energy ? ATTACK : RELEASE);

      context.clearRect(0, 0, width, height);
      context.fillStyle = cssColor(colorToken, "#4c8dff");

      // How many bars are still alight. The meter keeps working underneath —
      // the microphone is still open — so only the extinguished tail is still.
      const remaining = spanRef.current > 0
        ? (deadlineRef.current - Date.now()) / spanRef.current
        : 0;
      const lit = mode === "countdown"
        ? Math.max(0, Math.min(count, Math.ceil(remaining * count)))
        : count;

      for (let index = 0; index < count; index += 1) {
        const { phase, speed } = BARS[index];
        let target: number;
        if (mode === "thinking") {
          // A pulse travelling across otherwise still bars, so "working" never
          // reads as "hearing you".
          const centre = (Math.sin(time * 1.5) * 0.5 + 0.5) * (count - 1);
          const distance = Math.abs(index - centre);
          target = IDLE_LEVEL + 0.75 * Math.exp(-(distance * distance) / (count / 4));
        } else {
          // Rounded silhouette: the middle of the meter reaches higher.
          const fromCentre = Math.abs(index - middle) / (middle || 1);
          const shape = 1 - CENTER_DROP * Math.pow(fromCentre, 1.7);
          const shimmer = SHIMMER_FLOOR + (1 - SHIMMER_FLOOR) * Math.sin(time * speed + phase);
          // At rest the meter breathes: enough motion to read as live, far
          // too little to be mistaken for someone talking.
          const breathing = IDLE_LEVEL * (0.45 + 0.55 * Math.sin(time * 1.6 + phase));
          target = Math.max(breathing, energy * shape * shimmer);
        }
        const spent = index >= lit;
        if (spent) {
          target = 0;
        }
        // Per-bar smoothing on top of the shared envelope keeps neighbours
        // from snapping to the same value on a loud syllable.
        heights[index] += (target - heights[index]) * 0.35;

        const barHeight = Math.max(barWidth, Math.min(1, heights[index]) * height);
        const x = offset + index * step;
        const y = (height - barHeight) / 2;
        context.globalAlpha = spent ? 0.18 : 0.5 + 0.5 * (barHeight / height);
        context.beginPath();
        context.roundRect(x, y, barWidth, barHeight, barWidth / 2);
        context.fill();
      }
      context.globalAlpha = 1;
      raf = requestAnimationFrame(draw);
    };
    raf = requestAnimationFrame(draw);
    return () => cancelAnimationFrame(raf);
  }, [mode, height, barWidth, gap, colorToken, width]);

  return (
    <canvas
      ref={canvasRef}
      className={`wave ${className}`}
      aria-label={label}
      aria-hidden={label ? undefined : true}
      data-level={level.toFixed(2)}
    />
  );
}
