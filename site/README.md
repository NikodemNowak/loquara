# Loquara landing page

The public product page for Loquara: a concise product story, a live interface preview, and direct downloads for every platform. It is a static site built with Vite and plain HTML, CSS, and TypeScript.

- **Production URL:** https://loquara.nikodemnowak.pl
- **Output:** `site/dist/`
- **Not published automatically** by the app CI or release workflow

## Stack

- Vite 8 for the production build
- Plain HTML, CSS, and TypeScript; no client framework
- Self-hosted Schibsted Grotesk (page) and Inter (the app preview)
- No external font, analytics, image, or script requests at runtime

## Commands

```bash
pnpm install
pnpm site:dev       # development server, http://localhost:1421
pnpm site:build     # production build in site/dist
pnpm site:preview   # serve the production build at http://127.0.0.1:4173
```

Type-check the site source:

```bash
pnpm exec tsc -p site/tsconfig.json
```

## Structure

```text
site/
  index.html              page, bilingual copy, app preview, download section
  src/
    style.css             site layout and the app-window reconstruction
    main.ts               OS detection, download CTA labels, preview animation
    downloads.ts          release version, asset URLs, OS detection
  public/
    fonts/                Schibsted Grotesk and Inter (self-hosted)
    favicon.svg
    apple-touch-icon.png
    og.png                1200 x 630 social card
    robots.txt
    sitemap.xml
  vite.config.ts
  tsconfig.json
```

## Product page

The page is deliberately compact:

1. **Hero** with the product statement, a direct OS-specific download button, an "all downloads" link, and version/license/language context.
2. **Interface preview** with the real app shell and an animated dictation cycle.
3. **Download section** with version, release date, platform requirements, primary files, alternatives, and install notes.
4. **Footer** with source, releases, issues, license, and contact links.

The header carries a persistent download CTA and a version link to the GitHub release.

## Download behavior

The primary CTA and each platform's primary file link point directly at a GitHub release asset. Clicking starts the file download; there is no intermediate release page in the path. The release page is offered separately for older versions and release notes.

- Windows: `.exe` installer, `.msi` alternative
- macOS: universal `.dmg`
- Linux: `.deb`, `.AppImage` alternative

OS detection only changes the labels and the "This device" marker. Every file is always available to every visitor.

## Bilingual copy

Language is selected from the browser (`navigator.languages`) in the inline script in `index.html`. There is no language toggle.

- Visible copy uses paired `bi bi-en` / `bi bi-pl` spans. CSS shows the active language.
- The title, description, and dynamic app transcript are selected in JavaScript.
- `?lang=pl` and `?lang=en` are review-only overrides; they are not linked from the page.

When adding copy, add both language variants and keep the app strings aligned with `src/lib/i18n/messages.ts`.

## Theme

The site follows `prefers-color-scheme`; there is no manual theme switch. The embedded app preview is always dark because the current desktop application is dark-only (`color-scheme: dark` and `getCurrentWindow().setTheme("dark")`). This is intentional and should remain accurate until the app ships a light theme.

## Interface preview

The app window is an HTML/CSS reconstruction of the current Loquara shell at 980 x 680:

- title bar, top navigation, Dictate screen, and status bar
- the app's Inter typeface and dark tokens
- Polish/English strings from the app's message catalog
- two canvas level meters and a separate floating recorder

The demo loops through ready, recording, and pasting states. It starts when the figure enters the viewport, pauses when it leaves, and becomes a single static recording frame under `prefers-reduced-motion`. The transcript is sample content; the visible caption says so.

The preview is not a screenshot or a video. Keep it aligned with the app UI when the app changes.

## SEO and sharing

`index.html` includes:

- canonical URL and robots directives
- Open Graph and Twitter card metadata
- `SoftwareApplication` JSON-LD with version, license, author, and download URL
- self-hosted font preloads and a plain static document; no client-side metadata layer

`public/robots.txt` and `public/sitemap.xml` point at `https://loquara.nikodemnowak.pl/`. `public/og.png` is the 1200 x 630 social card. Regenerate it after a significant layout change.

The page uses two self-hosted fonts. Schibsted Grotesk is preloaded for the headline; Inter is preloaded for the visible app preview. No third-party requests are made.

## Release checklist

Before publishing a build:

1. Confirm the release tag and assets exist on GitHub.
2. Update the version, release date, file names, sizes, and URLs in:
   - `src/downloads.ts`
   - the static links in `index.html`
   - the download section copy and release date
   - `SoftwareApplication` JSON-LD
   - the footer version
   - this README and the repository README, if needed
3. Run `pnpm exec tsc -p site/tsconfig.json`.
4. Run `pnpm site:build`.
5. Run `pnpm site:preview` and check:
   - Polish and English (`?lang=pl`, `?lang=en`)
   - light and dark system themes
   - desktop, tablet, and mobile widths
   - every direct download starts a file download
   - no console errors
   - the interface preview animates and respects reduced motion
   - canonical, OG image, robots, sitemap, and JSON-LD
   - focus states, skip link, and contrast
6. Regenerate `public/og.png` if the hero or app preview changed.
7. Deploy `site/dist/` and smoke-test the production URL.

## Deployment

The site is designed for a static host. Cloudflare Pages fits the existing domain, but any static host works.

### Cloudflare Pages

- **Repository:** `NikodemNowak/loquara`
- **Build command:** `pnpm site:build`
- **Output directory:** `site/dist`
- **Root directory:** repository root
- **Node:** 22
- **Environment variables:** none

Add `loquara.nikodemnowak.pl` as a custom domain and let Cloudflare issue the TLS certificate. Verify that `/`, `/og.png`, `/robots.txt`, `/sitemap.xml`, and all direct download links respond with production URLs.

The app CI does not deploy the site. Publishing remains a deliberate manual step.

## License

The site's source is part of the MIT-licensed Loquara repository. Fonts are used under the SIL Open Font License; see `public/fonts/FONTS.md`.
