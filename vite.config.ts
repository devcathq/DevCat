import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
// @ts-expect-error type error without @types/node package
import process from "node:process";
// @ts-expect-error type error without @types/node package
import fs from "node:fs";
// @ts-expect-error type error without @types/node package
import { fileURLToPath } from "node:url";
const host = process.env.TAURI_DEV_HOST;

// The rooms whiteboard's fonts (Excalidraw), served by the app at /excalidraw/fonts/… instead of
// Excalidraw's CDN. Its CJK font (Xiaolai, 13 MB) is left out: Excalidraw fetches those glyphs
// from its CDN only when a board has Chinese, Japanese or Korean text.
function excalidrawFonts(): Plugin {
  const root: string = fileURLToPath(new URL("./node_modules/@excalidraw/excalidraw/dist/prod/fonts/", import.meta.url));
  const files = (dir = ""): string[] =>
    (fs.readdirSync(root + dir, { withFileTypes: true }) as { name: string; isDirectory(): boolean }[])
      .filter((e) => !(dir === "" && e.name === "Xiaolai"))
      .flatMap((e) => (e.isDirectory() ? files(`${dir}${e.name}/`) : [`${dir}${e.name}`]));
  return {
    name: "excalidraw-fonts",
    configureServer(server) {
      server.middlewares.use("/excalidraw/fonts/", (req, res, next) => {
        const rel = decodeURIComponent((req.url ?? "").split("?")[0]).replace(/^\/+/, "");
        if (!rel || rel.includes("..") || !files().includes(rel)) return next();
        res.setHeader("Content-Type", "font/woff2");
        res.end(fs.readFileSync(root + rel));
      });
    },
    generateBundle() {
      for (const rel of files()) this.emitFile({ type: "asset", fileName: `excalidraw/fonts/${rel}`, source: fs.readFileSync(root + rel) });
    },
  };
}

// https://vite.dev/config/
// `vite --mode bridge`: run the UI in a normal browser against the Rust devbridge (dev only).
// `vite --mode demo`: the same, plus made-up databases / VPNs / clusters (src/dev/demo.ts).
const bridgeAlias = Object.fromEntries(
  ["api/core", "api/app", "api/event", "api/window", "api/webviewWindow", "api/path", "plugin-dialog", "plugin-fs", "plugin-opener"]
    .map((m) => [`@tauri-apps/${m}`, new URL("./src/dev/bridge.ts", import.meta.url).pathname]),
);

export default defineConfig(({ mode }) => ({
  plugins: [react(), tailwindcss(), excalidrawFonts()],
  resolve: mode === "bridge" || mode === "demo" ? { alias: bridgeAlias } : undefined,
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 1421 } : undefined,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: { chunkSizeWarningLimit: 6000 },
}));
