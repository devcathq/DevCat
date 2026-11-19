// DevCat Mobile's look: dark, light, or following the phone's system setting (the default).
// Light sets `html.light` (index.css swaps the color tokens); terminals take their colors from
// `termTheme` and follow a change at once (`onTheme`).
import { create } from "zustand";

export type ThemeChoice = "system" | "dark" | "light";
const KEY = "devcat.mobile.theme";
const media = window.matchMedia("(prefers-color-scheme: light)");

function loadChoice(): ThemeChoice {
  const v = localStorage.getItem(KEY);
  return v === "dark" || v === "light" ? v : "system";
}

const resolve = (c: ThemeChoice) => (c === "system" ? (media.matches ? "light" : "dark") : c);

export const useTheme = create<{ choice: ThemeChoice; light: boolean }>(() => {
  const choice = loadChoice();
  return { choice, light: resolve(choice) === "light" };
});

function apply() {
  const light = resolve(useTheme.getState().choice) === "light";
  if (useTheme.getState().light !== light) useTheme.setState({ light });
  document.documentElement.classList.toggle("light", light);
  document.querySelector('meta[name="theme-color"]')?.setAttribute("content", light ? "#eef0f3" : "#0f1114");
}

export function setTheme(choice: ThemeChoice) {
  try { localStorage.setItem(KEY, choice); } catch { /* private mode */ }
  useTheme.setState({ choice });
  apply();
}

media.addEventListener("change", apply);
apply();

/** Terminal colors (xterm) for the dark or light look. */
export function termTheme(light: boolean) {
  return light
    ? {
      background: "#fbfbfc", foreground: "#1b1f27", cursor: "#c2410c", cursorAccent: "#fbfbfc", selectionBackground: "#c2410c38",
      black: "#1f2328", red: "#cf222e", green: "#116329", yellow: "#9a6700", blue: "#0969da", magenta: "#8250df", cyan: "#1b7c83", white: "#6e7781",
      brightBlack: "#57606a", brightRed: "#a40e26", brightGreen: "#1a7f37", brightYellow: "#7d4e00", brightBlue: "#218bff", brightMagenta: "#a475f9", brightCyan: "#3192aa", brightWhite: "#8c959f",
    }
    : {
      background: "#0a0b0d", foreground: "#e6e8ec", cursor: "#f5a524", cursorAccent: "#0a0b0d", selectionBackground: "#f5a52440",
      black: "#1b1e24", red: "#f87171", green: "#4ade80", yellow: "#fbbf24", blue: "#60a5fa", magenta: "#c084fc", cyan: "#22d3ee", white: "#c9cdd4",
      brightBlack: "#5c6370", brightRed: "#fca5a5", brightGreen: "#86efac", brightYellow: "#fde68a", brightBlue: "#93c5fd", brightMagenta: "#d8b4fe", brightCyan: "#67e8f9", brightWhite: "#f3f4f6",
    };
}
