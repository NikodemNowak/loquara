export type Os = "windows" | "macos" | "linux";

export type FileDownload = {
  label: string;
  file: string;
  size: string;
  primary?: boolean;
};

export const RELEASE = "0.10.0";

const BASE = `https://github.com/NikodemNowak/loquara/releases/download/v${RELEASE}/`;

export const DOWNLOADS: Record<Os, { name: string; files: FileDownload[] }> = {
  windows: {
    name: "Windows",
    files: [
      {
        label: "Installer",
        file: `Loquara_${RELEASE}_x64-setup.exe`,
        size: "17.6 MB",
        primary: true,
      },
      {
        label: "MSI package",
        file: `Loquara_${RELEASE}_x64_en-US.msi`,
        size: "22.3 MB",
      },
    ],
  },
  macos: {
    name: "macOS",
    files: [
      {
        label: "Disk image",
        file: `Loquara_${RELEASE}_universal.dmg`,
        size: "44.0 MB",
        primary: true,
      },
    ],
  },
  linux: {
    name: "Linux",
    files: [
      {
        label: "Debian package",
        file: `Loquara_${RELEASE}_amd64.deb`,
        size: "22.1 MB",
        primary: true,
      },
      {
        label: "AppImage",
        file: `Loquara_${RELEASE}_amd64.AppImage`,
        size: "93.0 MB",
      },
    ],
  },
};

export function downloadUrl(file: string): string {
  return `${BASE}${file}`;
}

export function primaryFile(os: Os): FileDownload {
  return DOWNLOADS[os].files.find((item) => item.primary) ?? DOWNLOADS[os].files[0];
}

export function primaryUrl(os: Os): string {
  return downloadUrl(primaryFile(os).file);
}

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
