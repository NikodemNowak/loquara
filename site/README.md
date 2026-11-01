# Loquara landing page

A compact product page: the headline, the app in motion, then the direct
downloads. The app preview sits above the download row so the product is
visible in the first viewport; the sticky header still carries an OS-specific
download button.

## The app preview

The window is an HTML/CSS reconstruction of the current Loquara shell at its
real 980 × 680 size: title bar, top navigation, Dictate view, status bar and the
separate floating recorder. It uses the app's own Inter font and the dark tokens
from `src/app/theme.css`, and the copy comes from the app's Polish/English
message catalog. It is not a screenshot and not a video.

The demo loops through the real states:

1. **Ready to dictate** with the shortcut and two recent transcripts.
2. **Recording** with the live level meter, elapsed time, cancel/stop controls
   and a word-by-word transcript.
3. **Pasting** with the clipboard confirmation.
4. Back to ready.

Two canvases draw the speech envelope: one in the main window (the app's pink
recording colour) and one in the floating overlay (white). The sequence starts
when the preview enters the viewport and pauses when it leaves. With
`prefers-reduced-motion`, it shows one complete recording frame instead of
looping. The transcript is sample content; the caption says so.

The current desktop app is **dark-only** (`color-scheme: dark` and
`getCurrentWindow().setTheme("dark")`), so the preview stays in that real dark
interface in both site themes. The site itself follows the system colour scheme;
there is no theme toggle.

## Language

The page picks Polish or English from the browser (`navigator.languages`), the
same way it picks the theme. There is no language switch. For reviewing both
versions, append `?lang=pl` or `?lang=en`; this override is only a testing aid.
The interface preview's strings, transcript, recent items, downloads and page
metadata all follow the selected language.

## Downloads

The header button detects the visitor's OS and points directly at that
platform's file. Below the preview, the download row exposes every primary file
and the secondary formats:

- `Loquara_0.10.0_x64-setup.exe` (Windows)
- `Loquara_0.10.0_x64_en-US.msi` (Windows, alternative)
- `Loquara_0.10.0_universal.dmg` (macOS)
- `Loquara_0.10.0_amd64.deb` (Linux)
- `Loquara_0.10.0_amd64.AppImage` (Linux, alternative)

Those links start a GitHub asset download directly; the release page is only in
the footer. When a new version lands, update `src/downloads.ts` and the static
links in `index.html`.

## Fonts and assets

- **Schibsted Grotesk** for the page, **Inter** for the app window; see
  `public/fonts/FONTS.md`.
- No Three.js, stock photography, generated 3D or external font requests.
- The app preview is a simulation; the interface structure and copy are taken
  from the app source.

## Build

```bash
pnpm site:dev      # http://localhost:1421
pnpm site:build    # writes site/dist/
pnpm exec vite preview --config site/vite.config.ts --host 127.0.0.1 --port 4173
```

The output is plain HTML, CSS, JS and fonts. It is intentionally not wired into
any deployment.
