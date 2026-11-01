# Loquara landing page

A light, technical transcript sheet for Loquara: a real spectrogram and a
scrubbable sample transcript, a floating recorder pill, the instrument specs,
and direct downloads for every release file. It is intentionally not wired into
any deployment: nothing in CI publishes it, and there is no Cloudflare, Vercel
or GitHub Pages configuration in the repository.

## Preview

```bash
pnpm site:dev      # http://localhost:1421
```

Or serve the production build locally:

```bash
pnpm site:build
pnpm exec vite preview --config site/vite.config.ts --host 127.0.0.1 --port 4173
```

## Download links

The buttons point straight at the GitHub release assets (`v0.10.0`), so a click
starts the download instead of opening a releases page:

- `Loquara_0.10.0_x64-setup.exe`
- `Loquara_0.10.0_x64_en-US.msi`
- `Loquara_0.10.0_universal.dmg`
- `Loquara_0.10.0_amd64.deb`
- `Loquara_0.10.0_amd64.AppImage`

The header and hero CTAs detect the visitor's OS and point at that platform's
primary file. The download table keeps every file visible and direct. When a
new release lands, update `src/downloads.ts` and the static links in
`index.html`.

## The sample

`public/demo.wav` is a synthesized voice sample (Microsoft Zira, Windows SAPI)
reading the three transcript lines. `public/spectrogram.png` is a log-frequency
spectrogram of that exact audio, so the playhead and transcript line up. The
page labels it as a synthesized voice; it is not a recording of a user.

## Assets

- **Archivo** and **Source Serif 4**, self-hosted WOFF2 from Google Fonts; see
  `public/fonts/FONTS.md`.
- No Three.js, no generated 3D, no stock photography.

## Build

```bash
pnpm site:build    # writes site/dist/
```

The output is plain HTML, CSS, JS, fonts and the two demo assets. Serve it from
the root of `loquara.nikodemnowak.pl` when it is time to publish.
