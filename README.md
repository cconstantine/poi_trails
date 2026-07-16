# Poi Trails

**Try it: https://cconstantine.github.io/poi_trails/**

A client-side (no backend) webcam mirror for practicing poi spinning, with a
light-trails mode that traces the path of bright poi heads through the air.
Built in Rust with [egui]/[eframe] (glow/WebGL2), compiled to WebAssembly and
served as a static site with [trunk].

- **Mirror mode** — horizontally-flipped live webcam feed.
- **Trails mode** — bright regions leave fading, color-preserving light trails.
  Tunable brightness threshold, brightness boost, fade duration, and background
  dim.
- **Suppress static background** — an adaptive background model so bright
  *static* clutter stops trailing while moving poi keep tracing.
- Settings (including the chosen camera) persist to `localStorage`.
- Installable as a PWA; once visited, it keeps working offline.

> The webcam requires a secure context: the app only works over **HTTPS** or
> `http://localhost` (browsers block `getUserMedia` otherwise).

## Develop

```sh
rustup target add wasm32-unknown-unknown
cargo install --locked trunk        # one-time
trunk serve --open                  # http://127.0.0.1:8080
```

`cargo test` runs the (host-native) trail-processing unit tests.
`cargo run` opens a native window with a synthetic test pattern (no camera),
handy for iterating on UI/trail behavior without a browser.

## Deploy (GitHub Pages)

`.github/workflows/deploy.yml` builds with trunk and publishes `dist/` to
GitHub Pages on every push to `main`. One-time setup: in the repo, go to
**Settings → Pages → Build and deployment → Source** and select
**GitHub Actions**.

The site is served from a subpath (`https://<user>.github.io/poi_trails/`), so
the workflow builds with `trunk build --release --public-url /poi_trails/`. If
you rename the repo or use a custom/root domain, update that flag accordingly
(root domain → `--public-url /`).

Any static host works too (`trunk build --release` then serve `dist/` over
HTTPS): Cloudflare Pages, Netlify, Vercel, etc.

## Analytics

Pageview counts are tracked with [GoatCounter] (no cookies, no personal data).
Dashboard: https://poi-trails.goatcounter.com

[egui]: https://github.com/emilk/egui
[eframe]: https://github.com/emilk/egui/tree/master/crates/eframe
[trunk]: https://trunkrs.dev
[GoatCounter]: https://www.goatcounter.com
