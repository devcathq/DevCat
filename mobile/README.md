# DevCat Mobile

A remote control for DevCat on your computer: watch AI agents (and get told when they finish or need you), start new chats, browse, preview and edit project files, browse and edit databases and Redis, switch VPNs and tunnels, and look after Kubernetes, from your phone.

Files (the **Files** tab, or **Browse files** in an agent's menu) uploads and downloads files, and copies a file's path relative to the project (⋯ menu) to paste into an agent's chat. It can't open folders with keys or credentials (`~/.ssh`, `~/.aws`, `~/.kube`, …) or DevCat's own data.

There's no DevCat server and no account. The phone talks to DevCat on your computer, encrypted end to end with keys created when you pair: **directly** on the same network, and from any other network (mobile data, another Wi-Fi) through a **relay**, switching to a direct path when the networks allow it (like RustDesk). The relay only forwards encrypted packets. By default these are [iroh](https://iroh.computer)'s free public relays; DevCat → 📱 → **Reach from other networks** can point at your own [iroh-relay](https://github.com/n0-computer/iroh/tree/main/iroh-relay) or turn relays off. Connection profiles and passwords stay on the computer; the phone only keeps its own key and the list of computers it paired with.

```
Phone (app or browser) ──QUIC, encrypted──► DevCat on the computer ──► databases, Redis, VPNs, clusters, agents
        │   same network: direct              ▲         │
        └── elsewhere: via the relay ─────────┘         │
ntfy app on the phone ◄──── push "finished / needs you" ┘  (optional, via ntfy.sh or your own ntfy server)
```

## Using it

1. On the computer: DevCat → **📱** (title bar, next to ☕) → turn **Remote access** on → **Show pairing code**.
2. Point the phone's **camera** at the QR code. It opens DevCat in the phone's browser:
   - **Open in DevCat app** hands the pairing to the app (tick "Always" to skip this page next time), or
   - **Continue in browser** pairs this browser; nothing to install.
   In the app you can also **Scan QR code** or paste the link.
3. On the computer: click **Allow** (full access or read-only).

### App or browser

Both have the same screens. With relays on, the QR code opens the browser version on GitHub Pages
(`https://devcathq.github.io/DevCat/`, over HTTPS). It reaches the computer through the relay with an iroh endpoint
compiled to WebAssembly (`mobile/web-iroh`), speaking the same protocol as the app, so it works on any network.
The pairing details are in the part of the link after `#`, which browsers never send to the page's server.

With relays off, the QR code opens the copy served by DevCat on the computer (TCP, same port number as the app's
UDP), which encrypts every message with a key from the QR code (protocol: `crates/remote-proto/src/web.rs`). That
page is plain HTTP, so someone who controls the Wi-Fi could alter it; the app has no such gap. Browsers also can't scan
codes in the page (use the camera app) or show notifications (use ntfy). DevCat → 📱 → **Allow phone browsers**
turns the browser version off.

Notifications: DevCat → 📱 → **Notifications**, install the free **ntfy** app and subscribe to the topic shown there. The app's **More → Notifications** has the same steps.

Both apps have a **Help** section for the usual problems (offline computer, firewall prompt, phone VPN, public address).

### Reaching the computer from outside the network

Nothing to do with relays on (the default): pair and use it anywhere. Without relays, forward the port shown in DevCat → 📱 (47100 by default; **UDP** for the app, **TCP** for browsers) on your router to the computer, enter your public IP or host name as **Public address** there, and pair again (or add the address in the app under More → Computer settings). Only paired phones are let in.

## Building

The frontend is `mobile/src` (React, built by `mobile/vite.config.ts`); the native side is `mobile/src-tauri` (Tauri 2). Both share the wire protocol in `crates/remote-proto` with the desktop app (`src-tauri/src/remote`).

The browser version is the same frontend built with `pnpm mobile:web` (→ `mobile/dist-web`, with `mobile/src/web` standing in for the Tauri APIs); the desktop build embeds it (`src-tauri/build.rs`). `pnpm mobile:check-crypto` checks the browser crypto against the Rust known answer.

The hosted version (GitHub Pages, `.github/workflows/pages.yml`): `mobile/web-iroh/build.sh` (WebAssembly; needs `rustup target add wasm32-unknown-unknown`, `rustup component add llvm-tools` on macOS, and `wasm-bindgen-cli` matching `mobile/web-iroh/Cargo.lock`), then `pnpm mobile:hosted` (→ `mobile/dist-hosted`). To try it against the demo host: `DEVCAT_DEMO_RELAY=public DEVCAT_DEMO_PAGE=http://127.0.0.1:8090/ cargo run --example remotedemo` and serve `mobile/dist-hosted` on port 8090.

Requirements: Node 22 + pnpm, Rust, and the platform SDKs below. Run commands from `mobile/`.

### Android

Android Studio (SDK, NDK, Java 17), then:

```sh
rustup target add aarch64-linux-android armv7-linux-androideabi i686-linux-android x86_64-linux-android
pnpm tauri android init        # once; writes src-tauri/gen/android
pnpm tauri android dev         # on a connected phone or emulator
pnpm tauri android build --apk # release APK (sign it; see .github/workflows/mobile.yml)
```

CI (`.github/workflows/mobile.yml`) builds an APK when it's run by hand from the Actions tab (releases don't build it for now; run it on a tag to attach the APK to that release). To install it: download `DevCat-Mobile.apk` (the workflow artifact, or the release asset) on the phone, open it, and allow "install unknown apps" for your browser once.

### iPhone

A Mac with a working Xcode, and an Apple ID (a free one installs on your own iPhone for 7 days; a paid Apple Developer account for longer and for TestFlight):

```sh
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
pnpm tauri ios init            # once; writes src-tauri/gen/apple
pnpm tauri ios dev --open      # opens Xcode: pick your team under Signing, run on the phone
```

`src-tauri/Info.ios.plist` adds the camera (QR code) and local network permission texts.

### Developing without a phone

The app also runs as a desktop window, and in a normal browser for UI work:

```sh
# a computer to talk to: real server and terminals, made-up databases / VPNs / clusters
(cd ../src-tauri && cargo run --example remotedemo)      # prints PAIR devcat://pair?…
# the phone side, as a desktop window
pnpm tauri dev
# or the phone side in a browser: the real Rust client behind a local HTTP bridge
(cd src-tauri && cargo run --example bridge)
pnpm mobile:bridge                                         # http://localhost:1442
```

`promo/mobile-capture.mjs` drives the browser version through every screen (used for screenshots). The demo host also serves the browser version: open the `WEB http://127.0.0.1:47199/#…` link it prints.
