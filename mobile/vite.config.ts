import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
// @ts-expect-error type error without @types/node package
import process from "node:process";
// @ts-expect-error type error without @types/node package
import { fileURLToPath } from "node:url";

// DevCat Mobile frontend (mobile/src).
// `--mode web`: the browser version DevCat serves to phone browsers (→ dist-web, embedded in the
//   desktop app by src-tauri/build.rs).
// `--mode hosted`: the browser version for any network, published on GitHub Pages (→ dist-hosted);
//   talks to the computer through its relay with mobile/web-iroh (build that first:
//   mobile/web-iroh/build.sh).
// `--mode bridge`: run it in a normal browser against the mobile bridge (`cargo run --example
//   bridge` in mobile/src-tauri) for UI testing.
// `--mode mock`: made-up data and no computer (src/dev/mock.ts), for designing screens.
const host = process.env.TAURI_DEV_HOST;
// fileURLToPath, not URL.pathname: on Windows the latter is "/D:/…".
const root = fileURLToPath(new URL(".", import.meta.url));
const bridge = fileURLToPath(new URL("./src/dev/bridge.ts", import.meta.url));
const mock = fileURLToPath(new URL("./src/dev/mock.ts", import.meta.url));
const web = fileURLToPath(new URL("./src/web/shim.ts", import.meta.url));
const irohPkg = fileURLToPath(new URL("./web-iroh/pkg/devcat_web_iroh.js", import.meta.url));
const shimmed = ["api/core", "api/event", "plugin-notification", "plugin-barcode-scanner", "plugin-opener", "plugin-deep-link"];

export default defineConfig(({ mode }) => ({
  root,
  plugins: [react(), tailwindcss()],
  // Hosted under a path (…github.io/DevCat/): relative asset URLs.
  base: mode === "hosted" ? "./" : "/",
  resolve: mode === "bridge" || mode === "mock" || mode === "web" || mode === "hosted"
    ? { alias: { ...Object.fromEntries(shimmed.map((m) => [`@tauri-apps/${m}`, mode === "bridge" ? bridge : mode === "mock" ? mock : web])), "devcat-web-iroh": irohPkg } }
    : undefined,
  clearScreen: false,
  server: {
    port: 1440,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 1441 } : undefined,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: { outDir: mode === "web" ? "dist-web" : mode === "hosted" ? "dist-hosted" : "dist", emptyOutDir: true, chunkSizeWarningLimit: 3000 },
}));
