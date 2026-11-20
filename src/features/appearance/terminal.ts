// Terminal behaviour settings (per computer, localStorage; shared by all DevCat windows, like the
// fonts): what selecting text does, whether programs may copy to the clipboard (OSC 52), and whether
// terminals draw on the app's background. Every terminal reads them when it needs them, so a change
// applies at once to the open tabs.
import type { ITheme } from "@xterm/xterm";
import { create } from "zustand";
import { onPaletteChange, palette } from "./store";
import { terminalTheme } from "./themes";

export interface TerminalPrefs {
  /** "select": releasing the mouse after selecting copies; "shortcut": selecting only highlights,
   *  ⌘C / Ctrl+C copies. */
  copy: "select" | "shortcut";
  /** Programs (Claude Code, tmux, vim, …) may copy to the clipboard with OSC 52. Writes only: reads are never answered. */
  osc52: boolean;
  /** Terminals have no background of their own: they show what's behind them (the work area, the window glass).
   *  Off: a solid terminal in the theme's work area color. */
  appBackground: boolean;
}

export const TERMINAL_DEFAULTS: TerminalPrefs = { copy: "select", osc52: true, appBackground: true };
const KEY = "devcat.terminal";

function load(): TerminalPrefs {
  try {
    return { ...TERMINAL_DEFAULTS, ...JSON.parse(localStorage.getItem(KEY) ?? "{}") };
  } catch {
    return { ...TERMINAL_DEFAULTS };
  }
}

interface Store extends TerminalPrefs {
  set: (p: Partial<TerminalPrefs>) => void;
}

export const useTerminalPrefs = create<Store>((set) => ({
  ...load(),
  set: (p) => set(p),
}));

export function terminalPrefs(): TerminalPrefs {
  const { copy, osc52, appBackground } = useTerminalPrefs.getState();
  return { copy, osc52, appBackground };
}

let fromOtherWindow = false;
useTerminalPrefs.subscribe(() => {
  if (fromOtherWindow) return;
  try { localStorage.setItem(KEY, JSON.stringify(terminalPrefs())); } catch { /* private mode */ }
});
window.addEventListener("storage", (e) => {
  if (e.key !== KEY) return;
  fromOtherWindow = true;
  useTerminalPrefs.setState(load());
  fromOtherWindow = false;
});

/** The xterm colors for now: the Appearance theme, on the app's background unless the user wants a solid terminal. */
export function currentTerminalTheme(): ITheme {
  return terminalTheme(palette(), !terminalPrefs().appBackground);
}

/** Keep a terminal's colors following the theme and the "app background" setting (`get`: the terminal, once it
 *  exists). Returns unsubscribe. */
export function followTerminalTheme(get: () => { options: { theme?: ITheme } } | undefined): () => void {
  const update = () => {
    const term = get();
    if (term) term.options.theme = currentTerminalTheme();
  };
  const offPalette = onPaletteChange(update);
  const offPrefs = useTerminalPrefs.subscribe((s, prev) => { if (s.appBackground !== prev.appBackground) update(); });
  return () => { offPalette(); offPrefs(); };
}
