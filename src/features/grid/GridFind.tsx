// Find in a DataGrid (Ctrl/⌘+F): searches every cell of every row. The grid renders only the rows
// on screen, so the webview's own find can't see the rest.
import clsx from "clsx";
import { ChevronDown, ChevronUp, Search, X } from "lucide-react";
import { useLayoutEffect, type RefObject } from "react";
import { IconButton } from "../../components/ui";
import type { CellValue } from "../../lib/types";
import { IS_MAC } from "../../lib/windows";

const MAX_MATCHES = 100_000;

export const FIND_KEY = IS_MAC ? "⌘F" : "Ctrl+F";

export interface FindMatches {
  /** Matching cells in view order, as `row * columns + column` (ascending). */
  keys: number[];
  set: Set<number>;
  /** Stopped at MAX_MATCHES. */
  capped: boolean;
}

const NONE: FindMatches = { keys: [], set: new Set(), capped: false };

/** Cells whose text contains `query` (ignoring case); `order` = data index of each view row. */
export function findMatches(order: number[], nCols: number, value: (d: number, c: number) => CellValue | undefined, query: string): FindMatches {
  const q = query.toLowerCase();
  if (!q) return NONE;
  const keys: number[] = [];
  for (let r = 0; r < order.length; r++) {
    for (let c = 0; c < nCols; c++) {
      const v = value(order[r], c);
      if (v === undefined || v === null) continue;
      if ((typeof v === "object" ? JSON.stringify(v) : String(v)).toLowerCase().includes(q)) {
        if (keys.length === MAX_MATCHES) return { keys, set: new Set(keys), capped: true };
        keys.push(r * nCols + c);
      }
    }
  }
  return { keys, set: new Set(keys), capped: false };
}

/** Index of the first key >= k (keys.length when there's none). */
export function firstFrom(keys: number[], k: number): number {
  let lo = 0, hi = keys.length;
  while (lo < hi) {
    const m = (lo + hi) >> 1;
    if (keys[m] < k) lo = m + 1;
    else hi = m;
  }
  return lo;
}

export function focusFind(input: HTMLInputElement | null) {
  input?.focus();
  input?.select();
}

export function FindBar({ inputRef, text, onText, matches, current, searching, onStep, onClose }: {
  inputRef: RefObject<HTMLInputElement | null>;
  text: string;
  onText: (t: string) => void;
  matches: FindMatches;
  /** Index of the selected match, -1 when the selected cell isn't one. */
  current: number;
  /** `text` hasn't been searched yet (typing). */
  searching: boolean;
  onStep: (dir: 1 | -1) => void;
  onClose: () => void;
}) {
  // focus on mount, before the next key arrives (keys typed right after Ctrl+F belong here)
  useLayoutEffect(() => focusFind(inputRef.current), [inputRef]);
  const n = matches.keys.length;
  const total = `${n.toLocaleString()}${matches.capped ? "+" : ""}`;
  const label = !text || searching ? "" : !n ? "No results" : current >= 0 ? `${(current + 1).toLocaleString()} of ${total}` : `${total} found`;
  return (
    <div className="flex h-8 shrink-0 items-center gap-1.5 border-b border-line bg-raised px-2 font-sans">
      <Search className="size-3.5 shrink-0 text-faint" />
      <input
        ref={inputRef}
        spellCheck={false}
        placeholder="Find in all rows"
        className="h-6 w-64 min-w-0 rounded border border-line bg-app px-2 text-xs text-fg outline-none placeholder:text-faint focus:border-accent/70"
        value={text}
        onChange={(e) => onText(e.target.value)}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === "Enter") { e.preventDefault(); onStep(e.shiftKey ? -1 : 1); }
          else if (e.key === "Escape") { e.preventDefault(); onClose(); }
          // not the webview's own find
          else if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "f") { e.preventDefault(); e.currentTarget.select(); }
        }}
      />
      <span className={clsx("min-w-20 text-[11px] whitespace-nowrap tabular-nums", n || !label ? "text-muted" : "text-danger")}>{label}</span>
      <IconButton title="Previous match (Shift+Enter)" disabled={!n} onClick={() => onStep(-1)}><ChevronUp className="size-3.5" /></IconButton>
      <IconButton title="Next match (Enter)" disabled={!n} onClick={() => onStep(1)}><ChevronDown className="size-3.5" /></IconButton>
      <div className="flex-1" />
      <IconButton title="Close (Esc)" onClick={onClose}><X className="size-3.5" /></IconButton>
    </div>
  );
}
