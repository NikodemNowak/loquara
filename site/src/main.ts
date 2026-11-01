import demo from "./demo.json";
import {
  DOWNLOADS,
  RELEASE,
  detectedOs,
  downloadUrl,
  primaryFile,
} from "./downloads";

function buildWave(host: HTMLElement, count: number) {
  for (let index = 0; index < count; index += 1) {
    const progress = count > 1 ? index / (count - 1) : 0.5;
    const envelope = Math.sin(progress * Math.PI);
    const noise = Math.abs(Math.sin(index * 12.9898 + 4.1) * 43758.5453) % 1;
    const bar = document.createElement("i");
    bar.style.setProperty("--h", (0.18 + envelope * (0.5 + noise * 0.35)).toFixed(3));
    bar.style.setProperty("--d", `${(index * 0.06).toFixed(2)}s`);
    host.append(bar);
  }
}

document.querySelectorAll<HTMLElement>("[data-wave]").forEach((host) => {
  buildWave(host, Number(host.dataset.wave) || 9);
});

const os = detectedOs();
if (os) {
  const primary = primaryFile(os);
  const label = `Download for ${DOWNLOADS[os].name}`;
  const short = `${DOWNLOADS[os].name} ${primary.label.toLowerCase()}, ${primary.size}.`;
  const full = `Version ${RELEASE}. ${short}`;

  document.querySelectorAll<HTMLAnchorElement>("[data-download-primary]").forEach((link) => {
    link.href = downloadUrl(primary.file);
    link.setAttribute("aria-label", `${label}: ${primary.label}, ${primary.size}`);
    const text = link.querySelector<HTMLElement>("[data-download-label]");
    if (text) {
      text.textContent = label;
    }
  });

  document.querySelectorAll<HTMLElement>("[data-download-meta]").forEach((meta) => {
    meta.textContent = meta.dataset.downloadMeta === "short" ? short : full;
  });

  document.querySelectorAll<HTMLElement>("[data-os]").forEach((row) => {
    row.classList.toggle("is-current", row.dataset.os === os);
  });
}

const session = document.querySelector<HTMLElement>(".session");
const audio = document.querySelector<HTMLAudioElement>("[data-audio]");
const seek = document.querySelector<HTMLInputElement>("[data-seek]");
const play = document.querySelector<HTMLButtonElement>("[data-play]");
const playLabel = document.querySelector<HTMLElement>("[data-play-label]");
const timeNodes = document.querySelectorAll<HTMLElement>("[data-time]");
const segmentButtons = document.querySelectorAll<HTMLButtonElement>("[data-segment]");
const plot = document.querySelector<HTMLElement>(".session__spectrogram");

const total = demo.duration;

function formatTime(value: number): string {
  const safe = Math.max(0, Math.min(total, value));
  const minutes = Math.floor(safe / 60);
  const seconds = Math.floor(safe % 60);
  return `${minutes}:${seconds.toString().padStart(2, "0")}`;
}

function render(time: number) {
  const clamped = Math.max(0, Math.min(total, time));
  const progress = total > 0 ? (clamped / total) * 100 : 0;

  session?.style.setProperty("--progress", `${progress.toFixed(3)}%`);
  if (seek) {
    seek.value = String(clamped);
  }
  timeNodes.forEach((node) => {
    node.textContent = `${formatTime(clamped)} / ${formatTime(total)}`;
  });

  segmentButtons.forEach((button) => {
    const index = Number(button.dataset.segment);
    const segment = demo.segments[index];
    const active = Boolean(segment) && clamped >= segment.start && clamped < segment.end;
    button.classList.toggle("is-active", active);
    if (active) {
      button.setAttribute("aria-current", "true");
    } else {
      button.removeAttribute("aria-current");
    }
  });
}

function seekTo(time: number) {
  const clamped = Math.max(0, Math.min(total, time));
  render(clamped);

  if (!audio) {
    return;
  }

  try {
    audio.currentTime = clamped;
  } catch {
    audio.addEventListener(
      "loadedmetadata",
      () => {
        audio.currentTime = clamped;
      },
      { once: true },
    );
  }
}

if (session && audio && seek && play && playLabel) {
  seek.max = String(total);

  const setPlaying = (playing: boolean) => {
    play.classList.toggle("is-playing", playing);
    play.setAttribute("aria-pressed", String(playing));
    playLabel.textContent = playing ? "Pause sample" : "Play sample";
  };

  audio.addEventListener("play", () => setPlaying(true));
  audio.addEventListener("pause", () => setPlaying(false));
  audio.addEventListener("timeupdate", () => render(audio.currentTime));
  audio.addEventListener("ended", () => {
    seekTo(0);
    setPlaying(false);
  });

  play.addEventListener("click", () => {
    if (audio.paused) {
      void audio.play().catch(() => undefined);
    } else {
      audio.pause();
    }
  });

  seek.addEventListener("input", () => {
    seekTo(Number(seek.value));
  });

  segmentButtons.forEach((button) => {
    button.addEventListener("click", () => {
      const index = Number(button.dataset.segment);
      const segment = demo.segments[index];
      if (!segment) {
        return;
      }
      seekTo(segment.start);
      if (audio.paused) {
        void audio.play().catch(() => undefined);
      }
    });
  });

  plot?.addEventListener("click", (event) => {
    const bounds = plot.getBoundingClientRect();
    if (bounds.width === 0) {
      return;
    }
    seekTo(((event.clientX - bounds.left) / bounds.width) * total);
  });

  render(0);
}
