// Go to File (Ctrl+P / ⌘P), like VS Code's and Cursor's quick open: fuzzy search over every file
// in the current folder (.gitignore respected), recent files first, Enter opens it in a tab.
import clsx from "clsx";
import { Search } from "lucide-react";
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { create } from "zustand";
import { Spinner } from "../../components/ui";
import { api } from "../../lib/api";
import { isRemote } from "../../lib/remotePath";
import { useTabs } from "../../store/tabs";
import { useWorkspace } from "../claude/sidebar/workspace";
import { FileIcon } from "./FileIcon";
import { openFilePreview } from "./format";

const SHOWN = 60;
const RECENT_KEY = "devcat.quickOpen.recent";
const MAX_RECENT = 30;

export const useQuickOpen = create<{ open: boolean }>(() => ({ open: false }));
export const openQuickOpen = () => useQuickOpen.setState({ open: true });
const close = () => useQuickOpen.setState({ open: false });

/** The folder to search: the active tab's folder, else the AI workspace's. */
function currentRoot(): string | null {
  const { tabs, activeId } = useTabs.getState();
  const t = tabs.find((x) => x.id === activeId);
  const fromTab = t && (t.type === "claude" || t.type === "terminal" ? t.cwd : t.type === "file" ? t.root : t.type === "git" ? t.repo : null);
  const root = fromTab || useWorkspace.getState().cwd;
  return root && !isRemote(root) ? root : null;
}

function recent(root: string): string[] {
  try {
    return (JSON.parse(localStorage.getItem(RECENT_KEY) ?? "{}") as Record<string, string[]>)[root] ?? [];
  } catch {
    return [];
  }
}
function remember(root: string, rel: string) {
  try {
    const all = JSON.parse(localStorage.getItem(RECENT_KEY) ?? "{}") as Record<string, string[]>;
    all[root] = [rel, ...(all[root] ?? []).filter((r) => r !== rel)].slice(0, MAX_RECENT);
    localStorage.setItem(RECENT_KEY, JSON.stringify(all));
  } catch { /* quota / private mode */ }
}

/** File lists per folder: shown at once from the last walk, refreshed in the background. */
const lists = new Map<string, { files: string[]; truncated: boolean }>();

const isBoundary = (s: string, i: number) => i === 0 || "/\\._- ".includes(s[i - 1]) || (s[i] >= "A" && s[i] <= "Z" && s[i - 1] >= "a" && s[i - 1] <= "z");

/** Fuzzy match of `q` (lowercase, no spaces) in `path`: letters in order, matched from the end so
 *  they land in the file name when they can. Higher score = better; null = no match. */
export function fuzzy(q: string, path: string): { score: number; hits: number[] } | null {
  const lower = path.toLowerCase();
  const hits: number[] = [];
  let j = lower.length - 1;
  for (let i = q.length - 1; i >= 0; i--) {
    while (j >= 0 && lower[j] !== q[i]) j--;
    if (j < 0) return null;
    hits.push(j--);
  }
  hits.reverse();
  const nameStart = path.lastIndexOf("/") + 1;
  const name = lower.slice(nameStart);
  // the extension counts little: `.tsx` shouldn't make `t` look like the start of a word
  const dot = lower.lastIndexOf(".");
  const ext = dot > nameStart ? dot : lower.length;
  let score = 0;
  hits.forEach((h, k) => {
    score += 1;
    if (k > 0 && h === hits[k - 1] + 1) score += 5;
    if (h > ext) return;
    if (isBoundary(path, h)) score += 6;
    if (h === nameStart) score += 8;
    if (h >= nameStart) score += 6;
  });
  if (name.startsWith(q)) score += 25;
  else if (name.includes(q)) score += 15;
  else if (lower.includes(q)) score += 8;
  return { score: score - path.length * 0.02 - (hits[hits.length - 1] - hits[0]) * 0.05, hits };
}

function Highlight({ text, from, hits }: { text: string; from: number; hits: Set<number> }) {
  const out: ReactNode[] = [];
  for (let i = 0; i < text.length; i++) {
    const on = hits.has(from + i);
    let k = i;
    while (k + 1 < text.length && hits.has(from + k + 1) === on) k++;
    const part = text.slice(i, k + 1);
    out.push(on ? <span key={i} className="font-semibold text-accent">{part}</span> : part);
    i = k;
  }
  return <>{out}</>;
}

export function QuickOpen() {
  const open = useQuickOpen((s) => s.open);
  return open ? <Palette /> : null;
}

function Palette() {
  const [root] = useState(currentRoot);
  const [query, setQuery] = useState("");
  const [list, setList] = useState(() => (root ? lists.get(root) ?? null : null));
  const [error, setError] = useState<string | null>(null);
  const [sel, setSel] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!root) return;
    let live = true;
    api.fsFindFiles(root).then(
      (l) => { lists.set(root, l); if (live) setList(l); },
      (e) => live && setError(String(e)),
    );
    return () => { live = false; };
  }, [root]);

  const results = useMemo(() => {
    if (!list || !root) return [];
    const q = query.toLowerCase().replace(/\s+/g, "");
    if (!q) {
      const have = new Set(list.files);
      const rec = recent(root).filter((r) => have.has(r));
      const rest = list.files.filter((f) => !rec.includes(f)).slice(0, SHOWN - rec.length);
      return [...rec, ...rest].slice(0, SHOWN).map((path) => ({ path, hits: [] as number[], recent: rec.includes(path) }));
    }
    const rec = new Set(recent(root));
    const scored: { path: string; score: number; hits: number[]; recent: boolean }[] = [];
    for (const path of list.files) {
      const m = fuzzy(q, path);
      if (m) scored.push({ path, score: m.score + (rec.has(path) ? 4 : 0), hits: m.hits, recent: rec.has(path) });
    }
    scored.sort((a, b) => b.score - a.score || a.path.length - b.path.length);
    return scored.slice(0, SHOWN);
  }, [list, query, root]);

  useEffect(() => setSel(0), [query]);
  useEffect(() => {
    listRef.current?.querySelector(`[data-i="${sel}"]`)?.scrollIntoView({ block: "nearest" });
  }, [sel]);

  const choose = (rel: string) => {
    if (!root) return;
    remember(root, rel);
    const sep = root.includes("\\") ? "\\" : "/";
    openFilePreview(root.replace(/[\\/]+$/, "") + sep + rel.split("/").join(sep), root);
    close();
  };
  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowDown" || (e.ctrlKey && e.key === "n")) { e.preventDefault(); setSel((s) => Math.min(s + 1, results.length - 1)); }
    else if (e.key === "ArrowUp" || (e.ctrlKey && e.key === "p")) { e.preventDefault(); setSel((s) => Math.max(s - 1, 0)); }
    else if (e.key === "Enter") { e.preventDefault(); if (results[sel]) choose(results[sel].path); }
    else if (e.key === "Escape") { e.preventDefault(); close(); }
  };

  const folder = root?.replace(/[\\/]+$/, "").split(/[\\/]/).pop();
  return (
    <div className="fixed inset-0 z-[80] flex justify-center bg-black/30 pt-[12vh]" onMouseDown={close}>
      <div className="flex h-fit max-h-[60vh] w-[640px] max-w-[92vw] flex-col overflow-hidden rounded-xl border border-line bg-panel shadow-2xl" onMouseDown={(e) => e.stopPropagation()}>
        <div className="flex items-center gap-2 border-b border-line px-3">
          <Search className="size-4 shrink-0 text-muted" />
          <input autoFocus value={query} onChange={(e) => setQuery(e.target.value)} onKeyDown={onKey} spellCheck={false}
            placeholder={folder ? `Go to file in ${folder}…` : "Go to file…"} aria-label="Go to file"
            className="h-11 min-w-0 flex-1 bg-transparent text-[14px] text-fg outline-none placeholder:text-faint" style={{ outline: "none", boxShadow: "none" }} />
          {root && !list && !error && <Spinner className="size-3.5" />}
        </div>
        <div ref={listRef} className="min-h-0 overflow-y-auto py-1">
          {!root ? (
            <Note>Open a folder first (AI workspace → pick a folder) to search its files.</Note>
          ) : error && !list ? (
            <Note>{error}</Note>
          ) : list && !results.length ? (
            <Note>{query ? "No matching files." : "This folder has no files."}</Note>
          ) : (
            results.map((r, i) => {
              const cut = r.path.lastIndexOf("/") + 1;
              const name = r.path.slice(cut), dir = r.path.slice(0, Math.max(cut - 1, 0));
              const hits = new Set(r.hits);
              return (
                <button key={r.path} data-i={i} onMouseMove={() => setSel(i)} onClick={() => choose(r.path)}
                  className={clsx("flex w-full min-w-0 items-center gap-2 px-3 py-1.5 text-left text-[13px]", i === sel ? "bg-accent/15" : "hover:bg-hover/60")}>
                  <FileIcon name={name} className="size-4 shrink-0" />
                  <span className="shrink-0 text-fg"><Highlight text={name} from={cut} hits={hits} /></span>
                  <span className="min-w-0 truncate text-[12px] text-faint"><Highlight text={dir} from={0} hits={hits} /></span>
                  {r.recent && <span className="ml-auto shrink-0 pl-2 text-[11px] text-faint">recently opened</span>}
                </button>
              );
            })
          )}
        </div>
        {list?.truncated && <div className="border-t border-line px-3 py-1.5 text-[11px] text-warn">Showing the first {list.files.length.toLocaleString()} files of this folder.</div>}
      </div>
    </div>
  );
}

function Note({ children }: { children: ReactNode }) {
  return <div className="px-3 py-3 text-[12.5px] text-muted">{children}</div>;
}
