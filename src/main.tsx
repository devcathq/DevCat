import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { CrashBoundary } from "./components/CrashScreen";
import "./index.css";
import { startAppearance } from "./features/appearance/store";
import { BUNDLED_MONO, startFonts } from "./features/appearance/fonts";
import "@fontsource-variable/jetbrains-mono";
import "@fontsource/dejavu-mono/400.css";
import "./lib/monaco";

startAppearance();
startFonts();

// `vite --mode demo`: lets promo/capture.mjs open screens directly (not in real builds).
if (import.meta.env.MODE === "demo") void import("./dev/demoHooks");

// Terminals measure the code font when they open and cache each glyph the first time they draw it:
// load the bundled fonts first (local files, quick; never wait more than a moment).
const fontReady = Promise.all([
  ...[400, 700].map((w) => document.fonts.load(`${w} 13px "${BUNDLED_MONO}"`)),
  document.fonts.load('13px "DejaVu Mono"', "↻▰▱"),
]);
void Promise.race([fontReady, new Promise((r) => setTimeout(r, 800))]).catch(() => {}).finally(() => {
  ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <React.StrictMode>
      <CrashBoundary>
        <App />
      </CrashBoundary>
    </React.StrictMode>,
  );
});
