// Keyboard shortcuts for split panes: Alt+Shift+⌘ (Ctrl elsewhere) + Backslash / Minus / arrows, matched
// on `e.code` like the AI workspace's ⌥⇧⌘1/2/3, so they work the same inside a terminal (which the
// terminals' key handlers let through, see TerminalPane) and on layouts where the character differs.
import { IS_MAC } from "./windows";

export type PaneKey = "splitRight" | "splitDown" | "left" | "right" | "up" | "down";

const CODES: Record<string, PaneKey> = {
  Backslash: "splitRight",
  Minus: "splitDown",
  ArrowLeft: "left",
  ArrowRight: "right",
  ArrowUp: "up",
  ArrowDown: "down",
};

export function paneKey(e: KeyboardEvent): PaneKey | null {
  if (!e.altKey || !e.shiftKey) return null;
  if (IS_MAC ? !e.metaKey || e.ctrlKey : !e.ctrlKey || e.metaKey) return null;
  return CODES[e.code] ?? null;
}

const MOD = IS_MAC ? "⌥⇧⌘" : "Ctrl+Alt+Shift+";
export const SPLIT_RIGHT_SHORTCUT = `${MOD}\\`;
export const SPLIT_DOWN_SHORTCUT = `${MOD}-`;
