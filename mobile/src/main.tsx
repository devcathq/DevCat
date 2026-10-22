import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import "./index.css";

// iOS lays the on-screen keyboard over the page instead of shrinking it: fit the app to what's visible.
const vv = window.visualViewport;
if (vv) {
  const fit = () => {
    // Pinch-zoomed: the visible area shrinks and pans, but the layout shouldn't follow it.
    if (vv.scale > 1.01) return;
    document.documentElement.style.setProperty("--app-h", `${vv.height}px`);
    window.scrollTo(0, 0);
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
