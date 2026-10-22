import type { Terminal } from "@xterm/xterm";

export const IS_MAC = navigator.platform.toLowerCase().includes("mac");

/** Copy / paste / clear / select all in an xterm terminal, shared by every terminal in the app:
 *  ⌘C ⌘V ⌘K ⌘A on macOS, Ctrl+Shift+C/V/K/A elsewhere (plain Ctrl+C/V stay with the shell).
 *  Call from `attachCustomKeyEventHandler` on keydown; true = handled, so return false to xterm. */
export function terminalEditKey(term: Terminal, e: KeyboardEvent): boolean {
  const mod = IS_MAC ? e.metaKey : e.ctrlKey && e.shiftKey;
  if (!mod) return false;
  if (e.code === "KeyC") {
    const sel = term.getSelection();
    if (sel) void navigator.clipboard.writeText(sel).catch(() => {});
    e.preventDefault();
    return true;
  }
  // ⌘V / Ctrl+Shift+V: let the native paste event reach xterm's textarea.
  if (e.code === "KeyV") return true;
  if (e.code === "KeyK") {
    e.preventDefault();
    term.clear();
    return true;
  }
  if (e.code === "KeyA") {
    e.preventDefault();
    term.selectAll();
    return true;
  }
  return false;
}
