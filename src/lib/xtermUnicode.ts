// Character widths for xterm: measure by grapheme cluster (Unicode 15), the way Claude Code, tmux and
// current terminals (iTerm2, Ghostty, WezTerm, Windows Terminal) do. With per-codepoint widths (the
// Unicode 11 addon) an emoji sequence gets the wrong number of cells: "⚠️" (base + VS16) 1 instead of 2,
// "👍🏽" (skin tone) 4, "👨‍👩‍👧" (ZWJ family) 6. The program then lays out the line at a different width
// than xterm draws it, and tables and boxes after the emoji come out skewed.
import { UnicodeGraphemesAddon } from "@xterm/addon-unicode-graphemes";
import type { Terminal } from "@xterm/xterm";

/** Call before `term.open()`. Needs `allowProposedApi`. */
export function loadUnicode(term: Terminal) {
  // The addon also makes its provider the active one ("15-graphemes").
  term.loadAddon(new UnicodeGraphemesAddon());
}
