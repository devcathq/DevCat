// Character widths for xterm: Unicode 11, plus emoji written with VS16 (U+FE0F) count as 2 cells.
// xterm's Unicode 11 table keeps "⚠️" / "✔️" / "ℹ️" at the base character's 1 cell, while Claude Code,
// tmux and other terminals (iTerm2, Ghostty, Windows Terminal) give them 2. The program then lays out
// the line one column wider than xterm draws it, and tables and boxes after the emoji come out skewed.
import { Unicode11Addon } from "@xterm/addon-unicode11";
import type { IUnicodeVersionProvider, Terminal } from "@xterm/xterm";

const VS16 = 0xfe0f;

// Same packing as xterm's UnicodeService: state << 3 | width << 1 | shouldJoin.
const widthOf = (props: number) => (props >> 1) & 3;
const withWidth = (props: number, width: number) => (props & ~0b110) | ((width & 3) << 1);

/** Call before `term.open()`. Needs `allowProposedApi`. */
export function loadUnicode(term: Terminal) {
  // The addon only hands its provider to `register`; catch it there to wrap it.
  let base: IUnicodeVersionProvider | undefined;
  new Unicode11Addon().activate({ unicode: { register: (p: IUnicodeVersionProvider) => { base = p; } } } as unknown as Terminal);
  const v11 = base!;
  term.unicode.register({
    version: "11-emoji",
    wcwidth: (cp) => v11.wcwidth(cp),
    charProperties(cp, preceding) {
      const props = v11.charProperties(cp, preceding);
      // VS16 joins the cell before it (shouldJoin bit); widen a 1-cell base to 2.
      return cp === VS16 && (props & 1) && widthOf(props) === 1 ? withWidth(props, 2) : props;
    },
  });
  term.unicode.activeVersion = "11-emoji";
}
