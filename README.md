# Poi Trails

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
- Settings persist to `localStorage`.

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

## Deploy (Cloudflare Pages)

Static output goes to `dist/` via `trunk build --release`. Cloudflare's build
image doesn't ship Rust, so the build command installs the toolchain itself.

In the Cloudflare Pages dashboard, connect this GitHub repo and set:

- **Framework preset:** None
- **Build command:**
  ```sh
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y -t wasm32-unknown-unknown && . "$HOME/.cargo/env" && curl -sL https://github.com/trunk-rs/trunk/releases/download/v0.21.14/trunk-x86_64-unknown-linux-gnu.tar.gz | tar -xzf - -C "$HOME/.cargo/bin" && trunk build --release
  ```
- **Build output directory:** `dist`

Served at the site root, so no `--public-url` override is needed. Every push to
the connected branch triggers a rebuild and deploy.

Any static host works too (`trunk build --release` then serve `dist/` over
HTTPS): GitHub Pages, Netlify, Vercel, etc.

[egui]: https://github.com/emilk/egui
[eframe]: https://github.com/emilk/egui/tree/master/crates/eframe
[trunk]: https://trunkrs.dev
