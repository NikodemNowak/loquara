import { useEffect, useRef, useState, type RefObject } from "react";

import { aboveRoom, cssColor, mixColour, prepareCanvas, resampleEnvelope, trackRoom } from "../lib/waveform";

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
const ATTACK = 0.55;
const RELEASE = 0.16;
/** Bars never fully collapse, so silence still looks like it is listening
 *  rather than like a row of dots. */
const IDLE_LEVEL = 0.16;
/** Where the bars change colour, and how fast they get there. */
const VOICE_THRESHOLD = 0.18;
const VOICE_EASE = 0.07;
/** How often a loudness sample is pushed into the scrolling envelope. */
const SAMPLE_MS = 18;

export interface LevelMeterProps {
  /** Current microphone level, `0..=1`. */
  level: number;
  /** Live level updated without re-rendering the parent. Preferred over `level`. */
  levelRef?: RefObject<number | null>;
  /** `thinking` shows activity with no microphone signal behind it.
   *  `countdown` keeps metering but puts the bars out from the right as a
   *  deadline approaches. */
  mode?: "live" | "thinking" | "countdown";
  /** Compact pill vs the larger recording window. */
  variant?: "compact" | "expanded";
  /** `countdown` only: epoch ms the last bar goes out at. */
  deadline?: number;
  /** `countdown` only: how long the whole row stands for, in ms. */
  span?: number;
  height: number;
  barWidth?: number;
  gap?: number;
  className?: string;
  label?: string;
  /** CSS custom property holding the bar colour while someone is talking. */
  colorToken?: string;
  /** Bar colour when the microphone is only hearing the room. */
  quietToken?: string;
}

/**
 * A microphone meter drawn from a short loudness history.
 *
 * Superwhisper / Wispr-style overlays do not paint a lockstep equaliser: they
 * scroll a true-ish envelope of recent speech. We only receive one RMS value,
 * so the honest picture is that history — not a FFT rainbow, and not a row of
 * sines sharing one amplitude.
 */
export function LevelMeter({
  level,
  levelRef,
  mode = "live",
  variant = "compact",
  deadline = 0,
  span = 1,
  height,
  barWidth = 3,
  gap = 3,
  className = "",
  label,
  colorToken = "--accent",
  quietToken = "--pill-muted",
}: LevelMeterProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const propLevelRef = useRef(level);
  propLevelRef.current = level;
  const deadlineRef = useRef(deadline);
  deadlineRef.current = deadline;
  const spanRef = useRef(span);
  spanRef.current = span;
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
    const count = Math.max(3, Math.floor((width + gap) / step));
    const contentWidth = count * step - gap;
    const offset = Math.max(0, (width - contentWidth) / 2);
    const heights = new Array<number>(count).fill(IDLE_LEVEL);
    const historyLen = variant === "expanded" ? 64 : 28;
    const history = new Array<number>(historyLen).fill(IDLE_LEVEL);
    const reduceMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    const loud = cssColor(colorToken, "#4c8dff");
    const quiet = cssColor(quietToken, "#8d96a1");
    let energy = 0;
    let floor = 0;
    let voice = 0;
    let lastPush = 0;
    let raf = 0;
    let running = true;

    const draw = (now: number) => {
      if (!running) return;
      if (document.hidden) {
        raf = 0;
        return;
      }
      const time = reduceMotion ? 0 : now / 1000;
      const raw = Math.max(0, levelRef?.current ?? propLevelRef.current);
      floor = trackRoom(raw, floor);
      const excess = aboveRoom(raw, floor);
      const shaped = mode === "thinking"
        ? 0.45
        : Math.min(1, Math.pow(excess * GAIN, LOUDNESS_EXPONENT));
      energy += (shaped - energy) * (shaped > energy ? ATTACK : RELEASE);
      voice += ((mode === "live" && shaped > VOICE_THRESHOLD ? 1 : 0) - voice) * VOICE_EASE;

      if (!reduceMotion && now - lastPush >= SAMPLE_MS) {
        history.shift();
        history.push(mode === "thinking" ? IDLE_LEVEL : Math.max(IDLE_LEVEL, energy));
        lastPush = now;
      } else if (reduceMotion) {
        history.fill(Math.max(IDLE_LEVEL, energy));
      }

      const remaining = spanRef.current > 0
        ? (deadlineRef.current - Date.now()) / spanRef.current
        : 0;
      const lit = mode === "countdown"
        ? Math.max(0, Math.min(count, Math.ceil(remaining * count)))
        : count;

      const envelope = resampleEnvelope(history, count);
      context.clearRect(0, 0, width, height);
      context.fillStyle = mode === "live" ? mixColour(quiet, loud, voice) : loud;

      for (let index = 0; index < count; index += 1) {
        let target: number;
        if (mode === "thinking") {
          // A tide, not a beat: processing must never read as "hearing you".
          const expanded = variant === "expanded";
          const speed = expanded ? 0.85 : 1.45;
          const widthBars = expanded ? count / 2.6 : count / 4.2;
          const centre = (Math.sin(time * speed) * 0.5 + 0.5) * (count - 1);
          const secondary = (Math.sin(time * speed * 0.53 + 1.7) * 0.5 + 0.5) * (count - 1);
          const primary = Math.exp(-((index - centre) ** 2) / Math.max(1, widthBars));
          const echo = expanded
            ? 0.45 * Math.exp(-((index - secondary) ** 2) / Math.max(1, widthBars * 1.4))
            : 0;
          target = IDLE_LEVEL + (expanded ? 0.82 : 0.7) * Math.max(primary, echo);
        } else {
          // Neighbours show different moments of the same speech, so they
          // cannot lock into a mechanical chorus.
          const lag = 1 - Math.min(0.22, index * 0.012);
          target = Math.max(IDLE_LEVEL, envelope[index] * lag);
        }
        const spent = index >= lit;
        if (spent) target = 0;
        heights[index] += (target - heights[index]) * (reduceMotion ? 1 : 0.38);

        const barHeight = Math.max(barWidth, Math.min(1, heights[index]) * height);
        const x = offset + index * step;
        const y = (height - barHeight) / 2;
        context.globalAlpha = spent ? 0.18 : 0.48 + 0.52 * (barHeight / height);
        context.beginPath();
        context.roundRect(x, y, barWidth, barHeight, barWidth / 2);
        context.fill();
      }
      context.globalAlpha = 1;
      raf = requestAnimationFrame(draw);
    };
    const onVisibility = () => {
      if (document.hidden) {
        if (raf) cancelAnimationFrame(raf);
        raf = 0;
      } else if (running && !raf) {
        raf = requestAnimationFrame(draw);
      }
    };
    document.addEventListener("visibilitychange", onVisibility);
    raf = requestAnimationFrame(draw);
    return () => {
      running = false;
      cancelAnimationFrame(raf);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [mode, variant, height, barWidth, gap, colorToken, quietToken, width, levelRef]);

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
