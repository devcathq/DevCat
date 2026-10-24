// Terminal behaviour settings (per computer, localStorage; shared by all DevCat windows, like the
// fonts): what selecting text does, and whether programs may copy to the clipboard (OSC 52). Every
// terminal reads them when it needs them, so a change applies at once to the open tabs.
import { create } from "zustand";

export interface TerminalPrefs {
  /** "select": releasing the mouse after selecting copies; "shortcut": selecting only highlights,
   *  ⌘C / Ctrl+C copies. */
  copy: "select" | "shortcut";
  /** Programs (Claude Code, tmux, vim, …) may copy to the clipboard with OSC 52. Writes only: reads are never answered. */
  osc52: boolean;
}

export const TERMINAL_DEFAULTS: TerminalPrefs = { copy: "select", osc52: true };
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
  const { copy, osc52 } = useTerminalPrefs.getState();
  return { copy, osc52 };
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
