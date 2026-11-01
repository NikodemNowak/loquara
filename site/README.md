# Loquara landing page

A static, single-page site for Loquara. It is intentionally not wired into any
deployment: nothing in CI publishes it, and there is no Cloudflare/Vercel/GitHub
Pages configuration in the repository.

## Preview

```bash
pnpm site:dev      # http://localhost:1421
```

## Build

```bash
pnpm site:build    # writes site/dist/
```

The output is plain HTML, CSS, JS, fonts and the Three.js chunk. Serve it from
the root of `loquara.nikodemnowak.pl` when it is time to publish.

## Notes

- The Three.js sculpture is loaded lazily and falls back to the CSS glow when
  WebGL is unavailable.
- `prefers-reduced-motion` disables the ribbon animation and scroll reveals.
- The app frame is a high-fidelity HTML mockup, not a screenshot of a real
  session, so it can be kept in sync with the interface without capturing a
  desktop.
- The download buttons point at the GitHub releases page. Replace them with
  direct asset links once the 0.10.0 release is published, if desired.
