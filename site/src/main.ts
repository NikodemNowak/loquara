import { PRIMARY, detectedOs } from "./downloads";

const lang = document.documentElement.lang === "pl" ? "pl" : "en";
const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

const TRANSCRIPT = {
  en: "Let’s ship the macOS build on Friday. I’ll take the release notes, and you check Linux.",
  pl: "Wypuśćmy build na macOS w piątek. Ja zajmę się notatkami do wydania, a Ty sprawdzisz Linuksa.",
};

function updateDownloadTarget() {
  const os = detectedOs();
  if (!os) {
    return;
  }

  const primary = PRIMARY[os];

  document.querySelectorAll<HTMLAnchorElement>("[data-download-primary]").forEach((link) => {
    link.href = primary.href;
    link.setAttribute(
      "aria-label",
      lang === "pl"
        ? `${primary.labelPl}: ${primary.file}, ${primary.size}`
        : `${primary.labelEn}: ${primary.file}, ${primary.size}`,
    );
    const en = link.querySelector<HTMLElement>("[data-cta-en]");
    const pl = link.querySelector<HTMLElement>("[data-cta-pl]");
    if (en) en.textContent = primary.labelEn;
    if (pl) pl.textContent = primary.labelPl;
  });

  document.querySelectorAll<HTMLElement>("[data-os]").forEach((row) => {
    row.classList.toggle("is-current", row.dataset.os === os);
  });
}

updateDownloadTarget();

type Meter = {
  start: () => void;
  stop: () => void;
  staticFrame: () => void;
};

function createMeter(
  canvas: HTMLCanvasElement,
  options: { barWidth: number; gap: number; loud: string; quiet: string },
): Meter {
  const context = canvas.getContext("2d");
  const history = new Array<number>(28).fill(0.16);
  let width = 1;
  let height = 1;
  let running = false;
  let frame = 0;
  let lastPush = 0;

  function measure() {
    const ratio = Math.min(2, window.devicePixelRatio || 1);
    const bounds = canvas.getBoundingClientRect();
    width = Math.max(1, Math.round(bounds.width || canvas.clientWidth || 44));
    height = Math.max(1, Math.round(bounds.height || canvas.clientHeight || 24));
    canvas.width = Math.max(1, Math.round(width * ratio));
    canvas.height = Math.max(1, Math.round(height * ratio));
    context?.setTransform(ratio, 0, 0, ratio, 0, 0);
  }

  function levelAt(time: number) {
    const t = time / 1000;
    const phrase = 0.55 + 0.45 * Math.sin(t * 1.35 + 0.8);
    const syllable = 0.5 + 0.5 * Math.sin(t * 8.7 + Math.sin(t * 2.3) * 1.7);
    const grain = 0.5 + 0.5 * Math.sin(t * 27.1 + 4.2);
    return Math.min(1, Math.max(0.08, 0.12 + phrase * syllable * 0.78 + grain * 0.12));
  }

  function draw() {
    if (!context) return;

    const step = options.barWidth + options.gap;
    const count = Math.max(3, Math.floor((width + options.gap) / step));
    const content = count * step - options.gap;
    const offset = Math.max(0, (width - content) / 2);
    const current = history[history.length - 1] ?? 0.2;

    context.clearRect(0, 0, width, height);
    context.fillStyle = current > 0.3 ? options.loud : options.quiet;

    for (let index = 0; index < count; index += 1) {
      const sampleIndex = Math.min(
        history.length - 1,
        Math.round((index / Math.max(1, count - 1)) * (history.length - 1)),
      );
      const value = Math.max(0.08, history[sampleIndex] ?? 0.16);
      const barHeight = Math.max(options.barWidth, value * height);
      const x = offset + index * step;
      const y = (height - barHeight) / 2;

      context.globalAlpha = 0.48 + 0.52 * (barHeight / height);
      context.beginPath();
      if (typeof context.roundRect === "function") {
        context.roundRect(x, y, options.barWidth, barHeight, options.barWidth / 2);
      } else {
        context.rect(x, y, options.barWidth, barHeight);
      }
      context.fill();
    }

    context.globalAlpha = 1;
  }

  function animate(now: number) {
    if (!running) return;
    if (!document.hidden) {
      if (now - lastPush > 48) {
        history.shift();
        history.push(levelAt(now));
        lastPush = now;
      }
      draw();
    }
    frame = requestAnimationFrame(animate);
  }

  return {
    start() {
      if (running) return;
      running = true;
      lastPush = 0;
      history.fill(0.16);
      measure();
      frame = requestAnimationFrame(animate);
    },
    stop() {
      running = false;
      cancelAnimationFrame(frame);
      frame = 0;
    },
    staticFrame() {
      measure();
      const shape = [0.2, 0.38, 0.72, 0.9, 0.54, 0.8, 0.42, 0.68, 0.86, 0.58, 0.3, 0.64, 0.78, 0.48];
      for (let index = 0; index < history.length; index += 1) {
        history[index] = shape[index % shape.length];
      }
      draw();
    },
  };
}

const stage = document.querySelector<HTMLElement>("[data-app-stage]");
const app = document.querySelector<HTMLElement>("[data-app-demo]");
const transcript = document.querySelector<HTMLElement>("[data-demo-transcript]");
const clock = document.querySelector<HTMLElement>("[data-demo-time]");
const appCanvas = document.querySelector<HTMLCanvasElement>('[data-demo-wave="app"]');
const overlayCanvas = document.querySelector<HTMLCanvasElement>('[data-demo-wave="overlay"]');

if (stage && app && transcript && clock && appCanvas && overlayCanvas) {
  const appMeter = createMeter(appCanvas, {
    barWidth: 2,
    gap: 2,
    loud: "#e29b9e",
    quiet: "#ababab",
  });
  const overlayMeter = createMeter(overlayCanvas, {
    barWidth: 2,
    gap: 2,
    loud: "#e5e5e5",
    quiet: "#969696",
  });

  const words = TRANSCRIPT[lang].split(/\s+/);
  const timers = new Set<number>();
  let active = false;
  let phase: "idle" | "recording" | "pasting" = "idle";
  let wordTimer = 0;
  let clockTimer = 0;
  let clockStarted = 0;

  const later = (callback: () => void, delay: number) => {
    const id = window.setTimeout(() => {
      timers.delete(id);
      callback();
    }, delay);
    timers.add(id);
    return id;
  };

  const clearTimers = () => {
    timers.forEach((id) => window.clearTimeout(id));
    timers.clear();
    window.clearTimeout(wordTimer);
    wordTimer = 0;
    window.clearInterval(clockTimer);
    clockTimer = 0;
  };

  const setPhase = (next: typeof phase) => {
    phase = next;
    app.dataset.phase = next;
    stage.dataset.phase = next;
  };

  const updateClock = () => {
    const elapsed = Math.max(0, Math.floor((performance.now() - clockStarted) / 1000));
    const minutes = String(Math.floor(elapsed / 60)).padStart(2, "0");
    const seconds = String(elapsed % 60).padStart(2, "0");
    clock.textContent = `${minutes}:${seconds}`;
  };

  const startClock = () => {
    window.clearInterval(clockTimer);
    clockStarted = performance.now();
    updateClock();
    clockTimer = window.setInterval(updateClock, 200);
  };

  const stopClock = () => {
    window.clearInterval(clockTimer);
    clockTimer = 0;
  };

  const clearDraft = () => {
    transcript.textContent = "";
    window.clearTimeout(wordTimer);
    wordTimer = 0;
    clock.textContent = "00:00";
  };

  const resetIdle = () => {
    clearTimers();
    clearDraft();
    app.dataset.clipboard = "pending";
    appMeter.stop();
    overlayMeter.stop();
    setPhase("idle");
  };

  const revealWords = () => {
    transcript.textContent = "";
    let index = 0;

    const step = () => {
      if (!active || phase !== "recording") return;
      index += 1;
      transcript.textContent = words.slice(0, index).join(" ");
      if (index < words.length) {
        wordTimer = window.setTimeout(step, 155 + (index % 4) * 35);
      }
    };

    wordTimer = window.setTimeout(step, 120);
  };

  const startRecording = () => {
    if (!active) return;

    setPhase("recording");
    app.dataset.clipboard = "pending";
    clearDraft();
    startClock();
    revealWords();
    appMeter.start();
    overlayMeter.start();

    const finishDelay = 900 + words.length * 210 + 900;
    later(() => {
      app.dataset.clipboard = "ready";
      setPhase("pasting");
      stopClock();
      appMeter.stop();
      overlayMeter.stop();
    }, finishDelay);

    later(() => {
      if (!active) return;
      resetIdle();
      later(startRecording, 2200);
    }, finishDelay + 1500);
  };

  if (reducedMotion) {
    app.dataset.clipboard = "ready";
    setPhase("recording");
    transcript.textContent = TRANSCRIPT[lang];
    clock.textContent = "00:06";
    appMeter.staticFrame();
    overlayMeter.staticFrame();
  } else {
    const start = () => {
      if (active) return;
      active = true;
      resetIdle();
      later(startRecording, 2200);
    };

    const stop = () => {
      active = false;
      resetIdle();
    };

    if (typeof IntersectionObserver === "undefined") {
      start();
    } else {
      const observer = new IntersectionObserver(
        ([entry]) => {
          if (entry.isIntersecting && entry.intersectionRatio >= 0.2) {
            start();
          } else {
            stop();
          }
        },
        { threshold: [0, 0.2, 0.5] },
      );

      observer.observe(stage);
    }
  }
}
