import type { Terminal } from "@xterm/xterm";

export const IS_MAC = navigator.platform.toLowerCase().includes("mac");

/** Copy / paste / clear / select all in an xterm terminal, shared by every terminal in the app:
 *  ⌘C ⌘V ⌘K ⌘A on macOS, Ctrl+Shift+C/V/K/A elsewhere. Elsewhere plain Ctrl+C / Ctrl+V also
 *  work like Windows Terminal: Ctrl+C copies a selection (and interrupts without one), Ctrl+V
 *  pastes. Call from `attachCustomKeyEventHandler` on keydown; true = handled, so return false to xterm. */
export function terminalEditKey(term: Terminal, e: KeyboardEvent): boolean {
  const plainCtrl = !IS_MAC && e.ctrlKey && !e.shiftKey && !e.altKey && !e.metaKey;
  const mod = IS_MAC ? e.metaKey : e.ctrlKey && e.shiftKey;
  if (plainCtrl && e.code === "KeyC" && term.hasSelection()) {
    void navigator.clipboard.writeText(term.getSelection()).catch(() => {});
    term.clearSelection();
    e.preventDefault();
    return true;
  }
  // Ctrl+V: let the native paste event reach xterm's textarea (text, or an image: see TerminalPane).
  if (plainCtrl && e.code === "KeyV") return true;
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

/** Selecting text with the mouse copies it (like Claude Code, PuTTY and iTerm2). Returns the cleanup. */
export function copyOnSelect(term: Terminal, el: HTMLElement): () => void {
  const onUp = () => {
    if (term.hasSelection()) void navigator.clipboard.writeText(term.getSelection()).catch(() => {});
  };
  el.addEventListener("mouseup", onUp);
  return () => el.removeEventListener("mouseup", onUp);
}
