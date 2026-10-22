// Font settings (per computer, localStorage; shared by all DevCat windows, not per project): the
// code font used by terminals and editors, terminal size / line height, an optional editor size,
// the UI font and the UI zoom. Applying sets `--font-mono` / `--font-sans` and the webview zoom;
// terminals and editors follow through `onFontsChange`.
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { create } from "zustand";
import { api } from "../../lib/api";

export interface Fonts {
  /** Code font family (terminals, editors); "" = the default stack. */
  mono: string;
  /** UI font family; "" = the default stack. */
  sans: string;
  terminalSize: number;
  terminalLineHeight: number;
  /** Editor font size; 0 = each editor's own size. */
  editorSize: number;
  /** Zoom of the whole window (1 = 100%). */
  uiZoom: number;
}

/** The code font bundled with the app (@fontsource-variable/jetbrains-mono, OFL), so the default
 *  looks the same on every computer; an installed JetBrains Mono still comes first. */
export const BUNDLED_MONO = "JetBrains Mono Variable";

/** Fallbacks after the chosen font. DejaVu Mono (bundled, @fontsource/dejavu-mono) comes right after
 *  JetBrains Mono: it has the symbols status lines use (↻ ▰ ▱ …) at the same cell width, where
 *  system symbol fonts draw them wider and overlap the next character. Cascadia Mono ships with
 *  Windows 11; the symbol fonts cover what is left. */
const MONO_FALLBACK = '"JetBrains Mono", "JetBrains Mono Variable", "DejaVu Mono", "Cascadia Mono", "SF Mono", ui-monospace, Menlo, Monaco, Consolas, "DejaVu Sans Mono", monospace, "Segoe UI Symbol", "Apple Symbols", "Noto Sans Symbols 2"';
const SANS_FALLBACK = '"Inter", ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif';

export const FONT_DEFAULTS: Fonts = { mono: "", sans: "", terminalSize: 13, terminalLineHeight: 1.15, editorSize: 0, uiZoom: 1 };
const KEY = "devcat.fonts";

/** Well-known fonts offered in the pickers when the installed ones can't be listed (only those found are shown). */
export const MONO_CHOICES = ["JetBrains Mono", "Cascadia Mono", "Cascadia Code", "Fira Code", "Source Code Pro", "Hack", "IBM Plex Mono",
  "Ubuntu Mono", "DejaVu Sans Mono", "Menlo", "SF Mono", "Monaco", "Consolas", "Lucida Console", "MesloLGS NF", "JetBrainsMono Nerd Font",
  "CaskaydiaCove Nerd Font", "FiraCode Nerd Font"];
export const SANS_CHOICES = ["Inter", "Segoe UI", "SF Pro Text", "Helvetica Neue", "Roboto", "Ubuntu", "Noto Sans", "Open Sans", "IBM Plex Sans", "Arial"];

function load(): Fonts {
  try {
    return { ...FONT_DEFAULTS, ...JSON.parse(localStorage.getItem(KEY) ?? "{}") };
  } catch {
    return { ...FONT_DEFAULTS };
  }
}

interface Store extends Fonts {
  set: (p: Partial<Fonts>) => void;
  reset: () => void;
}

export const useFonts = create<Store>((set) => ({
  ...load(),
  set: (p) => set(p),
  reset: () => set({ ...FONT_DEFAULTS }),
}));

const quote = (f: string) => (/^[\w-]+$/.test(f) && !/^\d/.test(f) ? f : `"${f.replace(/"/g, "")}"`);
const stack = (family: string, fallback: string) => (family.trim() ? `${quote(family.trim())}, ${fallback}` : fallback);

export function fonts(): Fonts {
  const { set: _s, reset: _r, ...f } = useFonts.getState();
  return f;
}
export const monoStack = (f: Fonts = fonts()) => stack(f.mono, MONO_FALLBACK);
export const sansStack = (f: Fonts = fonts()) => stack(f.sans, SANS_FALLBACK);

/** xterm options for the current fonts. */
export function terminalFont(f: Fonts = fonts()) {
  return { fontFamily: monoStack(f), fontSize: f.terminalSize, lineHeight: f.terminalLineHeight };
}

const TERMINAL_SIZES = { min: 8, max: 32 };
/** Terminal zoom (⌘/Ctrl + = / - / 0): one point bigger or smaller, or back to the default. */
export function zoomTerminal(step: 1 | -1 | 0) {
  const { terminalSize, set } = useFonts.getState();
  const next = step === 0 ? FONT_DEFAULTS.terminalSize : Math.round(terminalSize) + step;
  set({ terminalSize: Math.min(TERMINAL_SIZES.max, Math.max(TERMINAL_SIZES.min, next)) });
}

const ZOOM_KEYS: Record<string, 1 | -1 | 0> = { Equal: 1, NumpadAdd: 1, Minus: -1, NumpadSubtract: -1, Digit0: 0, Numpad0: 0 };
/** In a terminal's key handler: ⌘/Ctrl + = / - / 0 zoom the terminal font. Returns true when the key was a zoom
 *  key (the xterm handler should then return false). */
export function terminalZoomKey(e: KeyboardEvent): boolean {
  const mac = navigator.platform.toLowerCase().includes("mac");
  const mod = mac ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
  const step = ZOOM_KEYS[e.code];
  if (!mod || e.altKey || step === undefined) return false;
  e.preventDefault();
  zoomTerminal(step);
  return true;
}

/** UI zoom steps (the same as Chrome and VS Code). */
export const UI_ZOOMS = [0.5, 0.67, 0.75, 0.8, 0.9, 1, 1.1, 1.25, 1.5, 1.75, 2];
/** UI zoom (⌘/Ctrl + = / - / 0 outside terminals): the next step up or down, or back to 100%. */
export function zoomUi(step: 1 | -1 | 0) {
  const { uiZoom, set } = useFonts.getState();
  if (step === 0) return set({ uiZoom: 1 });
  const next = step > 0 ? UI_ZOOMS.find((z) => z > uiZoom + 0.001) : [...UI_ZOOMS].reverse().find((z) => z < uiZoom - 0.001);
  if (next) set({ uiZoom: next });
}

/** Monaco options for the current fonts (size only when the user set one). */
export function editorFont(f: Fonts = fonts()): { fontFamily: string; fontSize?: number } {
  return f.editorSize > 0 ? { fontFamily: monoStack(f), fontSize: f.editorSize } : { fontFamily: monoStack(f) };
}

export interface InstalledFont { name: string; mono: boolean }
let installedList: Promise<InstalledFont[] | null> | null = null;
/** The font families installed on this computer (asked once per window); null when they can't be listed. */
export function installedFonts(): Promise<InstalledFont[] | null> {
  installedList ??= api.fontsList().then((l) => (l.length ? l : null), () => null);
  return installedList;
}

const installed = new Map<string, boolean>();
/** Whether `family` is installed: text measures differently than with the generic fallbacks alone. Cached. */
export function fontInstalled(family: string): boolean {
  const known = installed.get(family);
  if (known !== undefined) return known;
  const ctx = document.createElement("canvas").getContext("2d");
  if (!ctx) return true;
  const sample = "mmmmmmmmmlli10OO@#";
  const found = ["monospace", "serif", "sans-serif"].some((g) => {
    ctx.font = `16px ${g}`;
    const base = ctx.measureText(sample).width;
    ctx.font = `16px ${quote(family)}, ${g}`;
    return ctx.measureText(sample).width !== base;
  });
  installed.set(family, found);
  return found;
}

const listeners = new Set<(f: Fonts) => void>();
/** Follow font changes (terminals, editors). Returns unsubscribe. */
export function onFontsChange(f: (fonts: Fonts) => void): () => void {
  listeners.add(f);
  return () => { listeners.delete(f); };
}

let last = "";
let lastZoom = 1;
function apply() {
  const f = fonts();
  const root = document.documentElement;
  root.style.setProperty("--font-mono", monoStack(f));
  root.style.setProperty("--font-sans", sansStack(f));
  if (f.uiZoom !== lastZoom) {
    lastZoom = f.uiZoom;
    void getCurrentWebviewWindow().setZoom(f.uiZoom).catch(() => {});
  }
  const key = JSON.stringify(f);
  if (key !== last) {
    last = key;
    listeners.forEach((l) => l(f));
  }
}

let started = false;
/** Apply now and keep following changes (this window and other windows). */
export function startFonts() {
  if (started) return;
  started = true;
  apply();
  let fromOtherWindow = false;
  useFonts.subscribe(() => {
    if (!fromOtherWindow) {
      try { localStorage.setItem(KEY, JSON.stringify(fonts())); } catch { /* private mode */ }
    }
    apply();
  });
  window.addEventListener("storage", (e) => {
    if (e.key !== KEY) return;
    fromOtherWindow = true;
    useFonts.setState(load());
    fromOtherWindow = false;
  });
}
