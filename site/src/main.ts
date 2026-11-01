import "./style.css";

const reduceMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

function buildWave(host: HTMLElement, count: number) {
  for (let index = 0; index < count; index += 1) {
    const progress = count > 1 ? index / (count - 1) : 0.5;
    const envelope = Math.sin(progress * Math.PI);
    const noise = Math.abs(Math.sin(index * 12.9898 + 4.1) * 43758.5453) % 1;
    const bar = document.createElement("i");
    const height = 0.14 + envelope * (0.5 + noise * 0.5);
    bar.style.setProperty("--h", height.toFixed(3));
    bar.style.setProperty("--d", `${(index * 0.035).toFixed(2)}s`);
    host.append(bar);
  }
}

document.querySelectorAll<HTMLElement>("[data-wave]").forEach((host) => {
  buildWave(host, Number(host.dataset.wave) || 32);
});

const revealItems = Array.from(document.querySelectorAll<HTMLElement>(".reveal"));
if (reduceMotion || !("IntersectionObserver" in window)) {
  revealItems.forEach((item) => item.classList.add("is-visible"));
} else {
  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (entry.isIntersecting) {
          entry.target.classList.add("is-visible");
          observer.unobserve(entry.target);
        }
      }
    },
    { rootMargin: "0px 0px -12% 0px", threshold: 0.12 },
  );
  revealItems.forEach((item) => observer.observe(item));
}

const userAgent = navigator.userAgent.toLowerCase();
const platform = userAgent.includes("mac")
  ? "macos"
  : userAgent.includes("win")
    ? "windows"
    : userAgent.includes("linux")
      ? "linux"
      : null;
if (platform) {
  document.querySelector(`[data-os="${platform}"]`)?.classList.add("is-current");
}

const header = document.querySelector<HTMLElement>(".site-header");
const syncHeader = () => header?.classList.toggle("is-scrolled", window.scrollY > 24);
syncHeader();
window.addEventListener("scroll", syncHeader, { passive: true });

const year = document.querySelector<HTMLElement>("[data-year]");
if (year) {
  year.textContent = String(new Date().getFullYear());
}

const canvas = document.querySelector<HTMLCanvasElement>("#ribbon");
if (canvas) {
  const load = () => {
    import("./scene")
      .then(({ startRibbon }) => startRibbon(canvas, reduceMotion))
      .catch(() => {
        // The static glow behind the canvas is the fallback for a browser
        // without WebGL; nothing else needs to happen.
      });
  };
  const idle = window as Window & {
    requestIdleCallback?: (callback: () => void, options?: { timeout: number }) => number;
  };
  if (idle.requestIdleCallback) {
    idle.requestIdleCallback(load, { timeout: 1200 });
  } else {
    window.setTimeout(load, 180);
  }
}
