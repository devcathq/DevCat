import type { Terminal } from "@xterm/xterm";
import { terminalPrefs } from "../features/appearance/terminal";
import { api } from "./api";
import { handleOsc52 } from "./osc52";

export const IS_MAC = navigator.platform.toLowerCase().includes("mac");
export const IS_WIN = navigator.platform.toLowerCase().startsWith("win");

/** Copy terminal text. The webview only allows a copy that a click or key started; a program's OSC 52
 *  copy arrives with its output, so when the webview refuses, the app puts the text on the clipboard. */
export function copyText(text: string) {
  void navigator.clipboard.writeText(text).catch(() => api.clipboardWriteText(text)).catch(() => {});
}

/** Copy / paste / clear / select all in an xterm terminal, shared by every terminal in the app:
 *  ⌘C ⌘V ⌘K ⌘A on macOS, Ctrl+Shift+C/V/K/A elsewhere. On Windows also plain Ctrl+C (only with a
 *  selection, else it's still ^C) and Ctrl+V, like Windows Terminal; on Linux they stay with the shell,
 *  except Ctrl+C with a selection when copying is set to "with a shortcut" (Appearance → Terminal).
 *  Call from `attachCustomKeyEventHandler` on keydown; true = handled, so return false to xterm. */
export function terminalEditKey(term: Terminal, e: KeyboardEvent): boolean {
  if (!IS_MAC && e.ctrlKey && !e.shiftKey && !e.altKey && !e.metaKey) {
    const ctrlCopies = IS_WIN || terminalPrefs().copy === "shortcut";
    if (e.code === "KeyC" && ctrlCopies && term.hasSelection()) {
      copyText(term.getSelection());
      // cleared, so the next Ctrl+C interrupts again
      term.clearSelection();
      e.preventDefault();
      return true;
    }
    if (IS_WIN && e.code === "KeyV") return true;
  }
  const mod = IS_MAC ? e.metaKey : e.ctrlKey && e.shiftKey;
  if (!mod) return false;
  if (e.code === "KeyC") {
    const sel = term.getSelection();
    if (sel) copyText(sel);
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

/** Selecting text with the mouse copies it (like Claude Code, PuTTY and iTerm2), unless copying is set
 *  to "with a shortcut" (read on each selection, so the setting applies to open terminals). Returns the cleanup. */
export function copyOnSelect(term: Terminal, el: HTMLElement): () => void {
  const onUp = () => {
    if (terminalPrefs().copy === "select" && term.hasSelection()) copyText(term.getSelection());
  };
  el.addEventListener("mouseup", onUp);
  return () => el.removeEventListener("mouseup", onUp);
}

/** Programs in the terminal (Claude Code, tmux, vim, …) copy with OSC 52 while "Allow programs to copy
 *  to the clipboard" is on. The clipboard is never read. Returns the cleanup. */
export function programCopy(term: Terminal): () => void {
  const h = handleOsc52(term, () => terminalPrefs().osc52, copyText);
  return () => h.dispose();
}
