import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import "./index.css";

// iOS lays the on-screen keyboard over the page instead of shrinking it: fit the app to what's
// visible. The app is pinned (position: fixed, index.css) to the visible area's top, so when iOS
// pans the page to show the caret the app moves with it. Don't scroll the page back: iOS pans
// again on the next keystroke, and that tug of war made the screen flicker on every character.
const vv = window.visualViewport;
if (vv) {
  const root = document.documentElement.style;
  let h = -1;
  let top = -1;
  const fit = (e?: Event) => {
    // Pinch-zoomed: the visible area shrinks and pans, but the layout shouldn't follow it.
    if (vv.scale > 1.01) return;
    const nh = Math.round(vv.height);
    const nt = Math.max(0, Math.round(vv.offsetTop));
    if (nt !== top) { top = nt; root.setProperty("--app-top", `${nt}px`); }
    if (nh === h) return;
    const shrank = nh < h;
    h = nh;
    root.setProperty("--app-h", `${nh}px`);
    // The keyboard just came up: bring the field being typed in into the smaller area.
    const el = document.activeElement;
    if (e && shrank && el instanceof HTMLElement && el.matches("input, textarea, [contenteditable]")) {
      requestAnimationFrame(() => el.scrollIntoView({ block: "nearest" }));
    }
  };
  vv.addEventListener("resize", fit);
  vv.addEventListener("scroll", fit);
  fit();
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
