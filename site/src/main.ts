import { PRIMARY, detectedOs } from "./downloads";

function buildWave(host: HTMLElement, count: number) {
  for (let index = 0; index < count; index += 1) {
    const progress = count > 1 ? index / (count - 1) : 0.5;
    const envelope = Math.sin(progress * Math.PI);
    const noise = Math.abs(Math.sin(index * 12.9898 + 4.1) * 43758.5453) % 1;
    const bar = document.createElement("i");
    bar.style.setProperty("--h", (0.16 + envelope * (0.52 + noise * 0.32)).toFixed(3));
    bar.style.setProperty("--d", `${(index * 0.045).toFixed(2)}s`);
    host.append(bar);
  }
}

document.querySelectorAll<HTMLElement>("[data-wave]").forEach((host) => {
  buildWave(host, Number(host.dataset.wave) || 16);
});

const os = detectedOs();
if (os) {
  const primary = PRIMARY[os];

  document.querySelectorAll<HTMLAnchorElement>("[data-download-primary]").forEach((link) => {
    link.href = primary.href;
    link.setAttribute("aria-label", `${primary.label}: ${primary.file}, ${primary.size}`);
    const text = link.querySelector<HTMLElement>("[data-download-label]");
    if (text) {
      text.textContent = primary.label;
    }
  });

  document.querySelectorAll<HTMLElement>("[data-os]").forEach((row) => {
    row.classList.toggle("is-current", row.dataset.os === os);
  });
}
