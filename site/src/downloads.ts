export type Os = "windows" | "macos" | "linux";

export const RELEASE = "0.10.0";

const BASE = `https://github.com/NikodemNowak/loquara/releases/download/v${RELEASE}/`;

export const PRIMARY: Record<
  Os,
  { label: string; href: string; file: string; size: string }
> = {
  windows: {
    label: "Download for Windows",
    href: `${BASE}Loquara_${RELEASE}_x64-setup.exe`,
    file: `Loquara_${RELEASE}_x64-setup.exe`,
    size: "17.6 MB",
  },
  macos: {
    label: "Download for macOS",
    href: `${BASE}Loquara_${RELEASE}_universal.dmg`,
    file: `Loquara_${RELEASE}_universal.dmg`,
    size: "44.0 MB",
  },
  linux: {
    label: "Download for Linux",
    href: `${BASE}Loquara_${RELEASE}_amd64.deb`,
    file: `Loquara_${RELEASE}_amd64.deb`,
    size: "22.1 MB",
  },
};

export function detectedOs(): Os | null {
  if (typeof navigator === "undefined") {
    return null;
  }

  const agent = navigator.userAgent;

  if (/iPhone|iPad|iPod|Android/i.test(agent)) {
    return null;
  }
  if (/Windows/i.test(agent)) {
    return "windows";
  }
  if (/Macintosh|Mac OS X/i.test(agent)) {
    return "macos";
  }
  if (/Linux|X11/i.test(agent)) {
    return "linux";
  }
  return null;
}
