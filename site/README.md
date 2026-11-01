# Loquara landing page

A deliberately small landing page: one headline, one short line about the
product, the three direct downloads, and one view of the app. It follows the
system colour scheme (`prefers-color-scheme`), so there is no theme toggle and
no stored preference.

## Preview

```bash
pnpm site:dev      # http://localhost:1421
```

Or serve the production build locally:

```bash
pnpm site:build
pnpm exec vite preview --config site/vite.config.ts --host 127.0.0.1 --port 4173
```

## Downloads

The header button detects the visitor's OS and points directly at that
platform's file. The download row exposes every primary file and the secondary
formats:

- `Loquara_0.10.0_x64-setup.exe` (Windows)
- `Loquara_0.10.0_x64_en-US.msi` (Windows, alternative)
- `Loquara_0.10.0_universal.dmg` (macOS)
- `Loquara_0.10.0_amd64.deb` (Linux)
- `Loquara_0.10.0_amd64.AppImage` (Linux, alternative)

Those links start a GitHub asset download directly; the release page is only in
the footer. When a new version lands, update `src/downloads.ts` and the static
links in `index.html`.

## Assets

- **Schibsted Grotesk**, self-hosted; see `public/fonts/FONTS.md`.
- The app window is a high-fidelity HTML/CSS mockup, not a screenshot of a real
  session.
- No Three.js, no stock photography, no generated 3D.

## Build

```bash
pnpm site:build    # writes site/dist/
```

The output is plain HTML, CSS, JS and the font. It is intentionally not wired
into any deployment.
