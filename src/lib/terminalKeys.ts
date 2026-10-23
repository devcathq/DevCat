import type { Terminal } from "@xterm/xterm";

export const IS_MAC = navigator.platform.toLowerCase().includes("mac");
export const IS_WIN = navigator.platform.toLowerCase().startsWith("win");

/** Copy / paste / clear / select all in an xterm terminal, shared by every terminal in the app:
 *  ⌘C ⌘V ⌘K ⌘A on macOS, Ctrl+Shift+C/V/K/A elsewhere. On Windows also plain Ctrl+C (only with a
 *  selection, else it's still ^C) and Ctrl+V, like Windows Terminal; on Linux they stay with the shell.
 *  Call from `attachCustomKeyEventHandler` on keydown; true = handled, so return false to xterm. */
export function terminalEditKey(term: Terminal, e: KeyboardEvent): boolean {
  if (IS_WIN && e.ctrlKey && !e.shiftKey && !e.altKey && !e.metaKey) {
    if (e.code === "KeyC" && term.hasSelection()) {
      void navigator.clipboard.writeText(term.getSelection()).catch(() => {});
      // cleared, so the next Ctrl+C interrupts again
      term.clearSelection();
      e.preventDefault();
      return true;
    }
    if (e.code === "KeyV") return true;
  }
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
