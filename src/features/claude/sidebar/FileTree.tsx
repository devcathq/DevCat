// Lazy, virtualized directory tree for the Claude workspace folder (git-status aware).
import { useVirtualizer } from "@tanstack/react-virtual";
import clsx from "clsx";
import {
  AlertTriangle, AtSign, ChevronRight, ChevronsDownUp, ClipboardCopy, ClipboardPaste, Copy, Eye, Files, EyeOff, FolderGit2, FolderSearch, GitBranch, History, Loader2, RefreshCw, Search,
  Pin, PinOff, SquareArrowOutUpRight, Trash2, UserSearch, X,
} from "lucide-react";
import { forwardRef, memo, useCallback, useDeferredValue, useEffect, useImperativeHandle, useMemo, useRef, useState, type KeyboardEvent, type MouseEvent } from "react";
import { toast } from "sonner";
import { IconButton, Spin } from "../../../components/ui";
import { api } from "../../../lib/api";
import type { FileEntry, GitStatus } from "../../../lib/types";
import { useTabs } from "../../../store/tabs";
import { useContextMenu, type MenuItem } from "../../connections/ContextMenu";
import { confirmDialog } from "../../connections/dialogs";
import { FileIcon } from "../../files/FileIcon";
import { basename, copyFile, copyText, dirname, openFilePreview, relPath, revealPath, REVEAL_LABEL } from "../../files/format";
import { openBlame, openGitRepo } from "../../git/open";
import { repoRootOf } from "../../git/repoOf";
import { useWorkspace } from "./workspace";
import { inFolder, isPinned, togglePin, unpin, unpinUnder, usePins } from "../../files/pins";
import { isRemote } from "../../../lib/remotePath";
import { DRAG_PATHS } from "../../files/format";
import { IS_MAC } from "../../../lib/windows";

const ROW_H = 22;
const INDENT = 12;

type Listing = { state: "loading" } | { state: "ok"; entries: FileEntry[] } | { state: "error"; message: string };

type Row =
  | { type: "entry"; entry: FileEntry; depth: number; expanded: boolean; loading: boolean }
  | { type: "note"; id: string; depth: number; text: string; error?: boolean; retry?: string };

export interface FileTreeHandle { refresh: () => void; collapseAll: () => void }

/** Most files a search shows; refine it for the rest. */
const MAX_HITS = 2000;

/** Every file under the folder (relative, .gitignore respected), for searching it. */
type AllFiles = { state: "loading" } | { state: "ok"; files: string[]; truncated: boolean } | { state: "error"; message: string };

/** The tree of the files (and folders) whose name matches `q`, with the folders leading to them
 *  open. `q` with a `/` matches the relative path instead. */
function searchRows(cwd: string, files: string[], q: string): { rows: Row[]; more: number } {
  const sep = cwd.includes("\\") && !cwd.includes("/") ? "\\" : "/";
  const base = cwd.replace(/[\\/]+$/, "");
  const byPath = q.includes("/");
  const hit = (rel: string) => (byPath ? rel : rel.slice(rel.lastIndexOf("/") + 1)).toLowerCase().includes(q);
  const dirs = new Set<string>();
  const shown = new Set<string>();
  const addParents = (rel: string) => {
    for (let i = rel.lastIndexOf("/"); i > 0; i = rel.lastIndexOf("/", i - 1)) {
      const d = rel.slice(0, i);
      if (dirs.has(d)) break;
      dirs.add(d);
    }
  };
  let more = 0;
  for (const f of files) {
    // a folder whose name matches shows too (without everything in it)
    for (let i = f.indexOf("/"); i > 0; i = f.indexOf("/", i + 1)) {
      const d = f.slice(0, i);
      if (!dirs.has(d) && hit(d)) { dirs.add(d); addParents(d); }
    }
    if (!hit(f)) continue;
    if (shown.size >= MAX_HITS) { more++; continue; }
    shown.add(f);
    addParents(f);
  }
  const children = new Map<string, { rel: string; isDir: boolean }[]>();
  const put = (rel: string, isDir: boolean) => {
    const i = rel.lastIndexOf("/");
    const parent = i < 0 ? "" : rel.slice(0, i);
    const list = children.get(parent) ?? [];
    list.push({ rel, isDir });
    children.set(parent, list);
  };
  for (const d of dirs) put(d, true);
  for (const f of shown) put(f, false);
  const rows: Row[] = [];
  const walk = (parent: string, depth: number) => {
    const entries = sortEntries((children.get(parent) ?? []).map(({ rel, isDir }) => ({
      name: rel.slice(rel.lastIndexOf("/") + 1), path: `${base}${sep}${rel.split("/").join(sep)}`, isDir, size: 0, ignored: false,
    })));
    for (const e of entries) {
      const rel = relOf(e.path, base, sep);
      const open = e.isDir && children.has(rel);
      rows.push({ type: "entry", entry: e, depth, expanded: open, loading: false });
      if (open) walk(rel, depth + 1);
    }
  };
  walk("", 0);
  return { rows, more };
}

const relOf = (abs: string, base: string, sep: string) => abs.slice(base.length + 1).split(sep).join("/");

function sortEntries(list: FileEntry[]): FileEntry[] {
  return [...list].sort((a, b) => (a.isDir === b.isDir ? a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" }) : a.isDir ? -1 : 1));
}

/** Map absolute path → git letter(s), and the set of directories containing changes. */
function useGitIndex(git: GitStatus | null) {
  return useMemo(() => {
    const files = new Map<string, string>();
    const dirs = new Set<string>();
    if (!git?.isRepo || !git.root) return { files, dirs };
    const root = git.root.replace(/[\\/]+$/, "");
    for (const f of git.files) {
      const abs = `${root}/${f.path.replace(/\/+$/, "")}`;
      const letter = f.index === "?" || f.worktree === "?" ? "??" : (f.worktree.trim() || f.index.trim() || "M");
      files.set(abs, letter);
      for (let i = abs.lastIndexOf("/"); i > 0; i = abs.lastIndexOf("/", i - 1)) {
        const d = abs.slice(0, i);
        if (dirs.has(d)) break;
        dirs.add(d);
      }
      // untracked directories are reported as "dir/" — mark them too
      if (f.path.endsWith("/")) dirs.add(abs);
    }
    return { files, dirs };
  }, [git]);
}

/** Open the Git tab of the repo containing `path`, filtered to that file. */
async function showFileHistory(path: string) {
  const root = await repoRootOf(dirname(path));
  if (root) openGitRepo(root, { path });
  else toast.error("This file isn't inside a git repository");
}

export function gitTone(letter?: string | null): string | undefined {
  if (!letter) return undefined;
  const l = letter.trim();
  if (l === "??" || l.includes("A")) return "text-ok";
  if (l.includes("D")) return "text-danger";
  if (l.includes("U")) return "text-danger";
  if (l.includes("M") || l.includes("R") || l.includes("C")) return "text-warn";
  return undefined;
}

const FileTree = forwardRef<FileTreeHandle, { cwd: string; git: GitStatus | null }>(function FileTree({ cwd, git }, ref) {
  const showIgnored = useWorkspace((s) => s.showIgnored);
  const setShowIgnored = useWorkspace((s) => s.setShowIgnored);
  const [listings, setListings] = useState<Record<string, Listing>>({});
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  const [selected, setSelected] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const query = useDeferredValue(filter.trim().toLowerCase());
  // searching reads every file under the folder (not just the open ones); SSH folders can't yet
  const [all, setAll] = useState<AllFiles | null>(null);
  // bumped when the folder changes or on Refresh: a search started before then is dropped
  const allGen = useRef(0);
  const searchAll = !isRemote(cwd);
  const activePath = useTabs((s) => {
    const t = s.tabs.find((x) => x.id === s.activeId);
    return t?.type === "file" ? t.path : null;
  });
  const gitIdx = useGitIndex(git);
  const allPins = usePins((s) => s.pins);
  const pins = useMemo(() => allPins.filter((p) => inFolder(p.path, cwd)), [allPins, cwd]);
  const menu = useContextMenu();
  const scrollRef = useRef<HTMLDivElement>(null);
  const gen = useRef(0);

  const load = useCallback(async (dir: string) => {
    const g = gen.current;
    setListings((l) => ({ ...l, [dir]: l[dir]?.state === "ok" ? l[dir] : { state: "loading" } }));
    try {
      const entries = await api.fsList(dir, showIgnored);
      if (g === gen.current) setListings((l) => ({ ...l, [dir]: { state: "ok", entries: sortEntries(entries) } }));
    } catch (e) {
      if (g === gen.current) setListings((l) => ({ ...l, [dir]: { state: "error", message: String(e) } }));
    }
  }, [showIgnored]);

  // New folder or ignored-toggle: start over (keep expanded dirs on toggle).
  useEffect(() => {
    gen.current++;
    setListings({});
    void load(cwd);
  }, [cwd, load]);
  useEffect(() => { allGen.current++; setExpanded(new Set()); setSelected(null); setFilter(""); setAll(null); }, [cwd]);
  useEffect(() => {
    if (!query || !searchAll || all) return;
    // (no cleanup: setting "loading" runs this again, and must not drop the search it started)
    const g = allGen.current;
    setAll({ state: "loading" });
    api.fsFindFiles(cwd).then(
      (r) => g === allGen.current && setAll({ state: "ok", files: r.files, truncated: r.truncated }),
      (e) => g === allGen.current && setAll({ state: "error", message: String(e) }),
    );
  }, [query, searchAll, all, cwd]);

  // Expanded dirs without a listing (after a reset) get loaded.
  useEffect(() => {
    for (const d of expanded) if (!listings[d]) void load(d);
  }, [expanded, listings, load]);

  const refresh = useCallback(() => {
    allGen.current++;
    setAll(null);
    void load(cwd);
    for (const d of expanded) void load(d);
  }, [cwd, expanded, load]);
  const collapseAll = useCallback(() => setExpanded(new Set()), []);

  // Paste the clipboard (files / folders copied in Explorer / Finder or with Copy here, or else an
  // image) into `dir`, then show it: the folder opens and reloads, and the first new item is selected.
  const [reveal, setReveal] = useState<string | null>(null);
  const paste = useCallback(async (dir: string) => {
    try {
      const pasted = await api.fsPaste(dir, cwd);
      if (dir !== cwd) setExpanded((s) => new Set(s).add(dir));
      await load(dir);
      if (pasted[0]) {
        setSelected(pasted[0]);
        setReveal(pasted[0]);
      }
      toast.success(pasted.length === 1 ? `Pasted ${basename(pasted[0])}` : `Pasted ${pasted.length} items`);
    } catch (e) {
      toast.error(String(e));
    }
  }, [cwd, load]);
  useImperativeHandle(ref, () => ({ refresh, collapseAll }), [refresh, collapseAll]);

  // Delete a file or folder: to the Trash on this computer, for good on an SSH host (no Trash there).
  const remove = useCallback(async (e: FileEntry) => {
    const remote = isRemote(e.path);
    const what = e.isDir ? `the folder ${e.name} and everything in it` : e.name;
    const ok = await confirmDialog({
      title: `Delete ${e.name}?`,
      message: remote ? `This deletes ${what} on the SSH host for good: there's no Trash there.` : `This moves ${what} to the ${IS_MAC ? "Trash" : "Recycle Bin"}, where you can put it back.`,
      confirmLabel: remote ? "Delete for good" : `Move to ${IS_MAC ? "Trash" : "Recycle Bin"}`,
      danger: true,
    });
    if (!ok) return;
    try {
      await api.fsDelete([e.path], cwd);
    } catch (err) {
      toast.error(String(err));
      return;
    }
    // its tabs close, and the tree shows the folder as it is now
    const under = (p: string) => p === e.path || p.startsWith(`${e.path}/`) || p.startsWith(`${e.path}\\`);
    const tabs = useTabs.getState();
    for (const t of tabs.tabs) if (t.type === "file" && under(t.path)) tabs.close(t.id, true);
    setExpanded((s) => new Set([...s].filter((d) => !under(d))));
    unpinUnder(e.path);
    setSelected(null);
    allGen.current++;
    setAll(null);
    const parent = dirname(e.path);
    void load(parent && parent !== e.path ? parent : cwd);
    toast.success(remote ? `Deleted ${e.name}` : `Moved ${e.name} to the ${IS_MAC ? "Trash" : "Recycle Bin"}`);
  }, [cwd, load]);

  const toggle = useCallback((dir: string, open?: boolean) => {
    setExpanded((s) => {
      const n = new Set(s);
      const want = open ?? !n.has(dir);
      if (want) n.add(dir); else n.delete(dir);
      return n;
    });
  }, []);

  const rows = useMemo<Row[]>(() => {
    const out: Row[] = [];
    const q = query;
    if (q && all?.state === "loading") return [{ type: "note", id: "#searching", depth: 0, text: "Searching…" }];
    if (q && all?.state === "ok") {
      const { rows: hits, more } = searchRows(cwd, all.files, q);
      if (more) hits.push({ type: "note", id: "#more", depth: 0, text: `${more.toLocaleString()} more matches: type more of the name` });
      if (all.truncated && hits.length) hits.push({ type: "note", id: "#truncated", depth: 0, text: "Searched the first 100,000 files only" });
      return hits;
    }
    // Filtering walks every loaded node and keeps matches + their ancestors.
    const walk = (dir: string, depth: number, into: Row[]): boolean => {
      const l = listings[dir];
      if (!l || l.state === "loading") {
        if (!q) into.push({ type: "note", id: `${dir}#loading`, depth, text: "Loading…" });
        return false;
      }
      if (l.state === "error") {
        if (!q) into.push({ type: "note", id: `${dir}#error`, depth, text: l.message, error: true, retry: dir });
        return false;
      }
      if (!q && l.entries.length === 0) into.push({ type: "note", id: `${dir}#empty`, depth, text: "Empty folder" });
      let any = false;
      for (const e of l.entries) {
        const isOpen = e.isDir && (q ? !!listings[e.path] && listings[e.path].state === "ok" : expanded.has(e.path));
        const row: Row = { type: "entry", entry: e, depth, expanded: isOpen, loading: e.isDir && expanded.has(e.path) && listings[e.path]?.state === "loading" };
        if (!q) {
          into.push(row);
          if (e.isDir && expanded.has(e.path)) walk(e.path, depth + 1, into);
          continue;
        }
        const sub: Row[] = [];
        const childHit = e.isDir && isOpen ? walk(e.path, depth + 1, sub) : false;
        if (childHit || e.name.toLowerCase().includes(q)) {
          into.push(row, ...sub);
          any = true;
        }
      }
      return any;
    };
    walk(cwd, 0, out);
    return out;
  }, [cwd, listings, expanded, query, all]);

  const virt = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_H,
    overscan: 12,
  });

  // scroll to what was just pasted once its row is there
  useEffect(() => {
    if (!reveal) return;
    const i = rows.findIndex((r) => r.type === "entry" && r.entry.path === reveal);
    if (i >= 0) {
      virt.scrollToIndex(i);
      setReveal(null);
    }
  }, [reveal, rows, virt]);

  const rowKey = (r: Row) => (r.type === "entry" ? r.entry.path : r.id);
  const selIndex = selected ? rows.findIndex((r) => r.type === "entry" && r.entry.path === selected) : -1;

  // a folder in search results: leave the search and show it open in the tree
  const showDir = useCallback((dir: string) => {
    const base = cwd.replace(/[\\/]+$/, "");
    const sep = dir.charAt(base.length) || "/";
    setExpanded((s) => {
      const n = new Set(s);
      for (let i = dir.indexOf(sep, base.length + 1); i > 0; i = dir.indexOf(sep, i + 1)) n.add(dir.slice(0, i));
      n.add(dir);
      return n;
    });
    setFilter("");
    setReveal(dir);
  }, [cwd]);

  const activate = useCallback((e: FileEntry) => {
    setSelected(e.path);
    if (e.isDir && query && all?.state === "ok") showDir(e.path);
    else if (e.isDir) toggle(e.path);
    else openFilePreview(e.path, cwd);
  }, [cwd, toggle, query, all, showDir]);

  const onKey = (ev: KeyboardEvent<HTMLDivElement>) => {
    if ((ev.target as HTMLElement).tagName === "INPUT") return;
    const cur = selIndex >= 0 ? rows[selIndex] : null;
    const move = (i: number) => {
      for (let j = i; j >= 0 && j < rows.length; j += i >= selIndex ? 1 : -1) {
        const r = rows[j];
        if (r.type === "entry") { setSelected(r.entry.path); virt.scrollToIndex(j); return; }
      }
    };
    if (ev.key === "ArrowDown") { ev.preventDefault(); move(selIndex + 1); }
    else if (ev.key === "ArrowUp") { ev.preventDefault(); move(Math.max(0, selIndex - 1)); }
    else if (cur?.type === "entry" && ev.key === "Enter") { ev.preventDefault(); activate(cur.entry); }
    else if (cur?.type === "entry" && (ev.key === "Delete" || (IS_MAC && ev.key === "Backspace" && ev.metaKey))) {
      ev.preventDefault();
      void remove(cur.entry);
    }
    else if (cur?.type === "entry" && ev.code === "KeyC" && (IS_MAC ? ev.metaKey : ev.ctrlKey) && !ev.shiftKey && !ev.altKey && !isRemote(cur.entry.path)) {
      ev.preventDefault();
      void copyFile(cur.entry.path);
    }
    else if (ev.code === "KeyV" && (IS_MAC ? ev.metaKey : ev.ctrlKey) && !ev.shiftKey && !ev.altKey) {
      // into the selected folder, the selected file's folder, or the top
      const dir = cur?.type === "entry" ? (cur.entry.isDir ? cur.entry.path : dirname(cur.entry.path)) : cwd;
      if (!isRemote(dir)) {
        ev.preventDefault();
        void paste(dir);
      }
    }
    else if (cur?.type === "entry" && ev.key === "ArrowRight" && cur.entry.isDir) {
      ev.preventDefault();
      if (!cur.expanded) toggle(cur.entry.path, true); else move(selIndex + 1);
    } else if (cur?.type === "entry" && ev.key === "ArrowLeft") {
      ev.preventDefault();
      if (cur.entry.isDir && cur.expanded) toggle(cur.entry.path, false);
      else for (let j = selIndex - 1; j >= 0; j--) {
        const r = rows[j];
        if (r.type === "entry" && r.depth < cur.depth) { setSelected(r.entry.path); virt.scrollToIndex(j); break; }
      }
    }
  };

  const showMenu = useCallback((ev: MouseEvent, e: FileEntry) => {
    setSelected(e.path);
    const rel = relPath(e.path, cwd);
    const items: MenuItem[] = [
      e.isDir
        ? { label: expanded.has(e.path) ? "Collapse" : "Expand", icon: <ChevronRight className="size-3.5" />, onClick: () => toggle(e.path) }
        : { label: "Open preview", icon: <SquareArrowOutUpRight className="size-3.5" />, onClick: () => openFilePreview(e.path, cwd) },
      ...(e.isDir ? [] : [isPinned(usePins.getState().pins, e.path)
        ? { label: "Unpin", icon: <PinOff className="size-3.5" />, onClick: () => unpin(e.path) }
        : { label: "Pin", icon: <Pin className="size-3.5" />, onClick: () => togglePin(e.path, cwd) }]),
      ...(e.isDir
        ? e.isRepo && !isRemote(e.path) ? [{ label: "Open Git repository", icon: <FolderGit2 className="size-3.5" />, onClick: () => openGitRepo(e.path) }] : []
        : isRemote(e.path) ? [] : [
          { label: "Blame", icon: <UserSearch className="size-3.5" />, onClick: () => openBlame(e.path, cwd) },
          { label: "File history", icon: <History className="size-3.5" />, onClick: () => void showFileHistory(e.path) },
        ]),
      "separator",
      ...(isRemote(e.path) ? [] : [
        { label: "Copy", shortcut: IS_MAC ? "⌘C" : "Ctrl+C", icon: <Files className="size-3.5" />, onClick: () => void copyFile(e.path) },
        { label: "Paste", shortcut: IS_MAC ? "⌘V" : "Ctrl+V", icon: <ClipboardPaste className="size-3.5" />, onClick: () => void paste(e.isDir ? e.path : dirname(e.path)) },
      ]),
      { label: "Copy path", icon: <Copy className="size-3.5" />, onClick: () => void copyText(e.path) },
      { label: "Copy relative path", icon: <ClipboardCopy className="size-3.5" />, onClick: () => void copyText(rel) },
      { label: "Mention in chat", icon: <AtSign className="size-3.5" />, onClick: () => void copyText(`@${rel}${e.isDir ? "/" : ""}`, `Copied @${rel} — paste it in the chat`) },
      ...(isRemote(e.path) ? [] : ["separator", { label: REVEAL_LABEL, icon: <FolderSearch className="size-3.5" />, onClick: () => void revealPath(e.path) }] as MenuItem[]),
      "separator",
      { label: "Delete…", shortcut: IS_MAC ? "⌘⌫" : "Del", danger: true, icon: <Trash2 className="size-3.5" />, onClick: () => void remove(e) },
    ];
    menu.show(ev, items);
  }, [cwd, expanded, toggle, menu, paste, remove]);

  const root = listings[cwd];

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex h-7 shrink-0 items-center gap-1 px-2">
        <div className="relative min-w-0 flex-1">
          <Search className="pointer-events-none absolute top-1/2 left-1.5 size-3 -translate-y-1/2 text-faint" />
          <input
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            onKeyDown={(e) => e.key === "Escape" && setFilter("")}
            placeholder={searchAll ? "Search files" : "Filter loaded files"}
            spellCheck={false}
            className="h-5.5 w-full rounded border border-line bg-app pr-5 pl-6 text-[12px] placeholder:text-faint focus:border-accent focus:outline-none"
          />
          {filter && (
            <button className="absolute top-1/2 right-1 -translate-y-1/2 text-faint hover:text-fg" onClick={() => setFilter("")} aria-label="Clear filter">
              <X className="size-3" />
            </button>
          )}
        </div>
        <IconButton title={showIgnored ? "Hide ignored files" : "Show ignored files"} active={showIgnored} onClick={() => setShowIgnored(!showIgnored)} className="size-5.5">
          {showIgnored ? <Eye className="size-3.5" /> : <EyeOff className="size-3.5" />}
        </IconButton>
        <IconButton title="Collapse all" onClick={collapseAll} className="size-5.5"><ChevronsDownUp className="size-3.5" /></IconButton>
        <IconButton title="Refresh" onClick={refresh} className="size-5.5">
          <Spin icon={RefreshCw} spinning={root?.state === "loading"} className="size-3.5" />
        </IconButton>
      </div>
      {pins.length > 0 && !query && <PinnedList pins={pins} cwd={cwd} activePath={activePath} onMenu={showMenu} />}
      <div
        ref={scrollRef}
        tabIndex={0}
        onKeyDown={onKey}
        className="min-h-0 flex-1 overflow-auto outline-none focus-visible:outline-none"
      >
        {query && rows.length === 0 ? (
          <div className="px-3 py-2 text-[12px] text-faint italic">
            {all?.state === "ok" ? `No file matches “${filter}”.` : `No loaded file matches “${filter}”. Expand folders to search deeper.`}
          </div>
        ) : (
          <div style={{ height: virt.getTotalSize(), position: "relative" }}>
            {virt.getVirtualItems().map((vi) => {
              const r = rows[vi.index];
              return (
                <div key={rowKey(r)} style={{ position: "absolute", top: 0, left: 0, right: 0, height: ROW_H, transform: `translateY(${vi.start}px)` }}>
                  {r.type === "note" ? (
                    <NoteRow row={r} onRetry={load} />
                  ) : (
                    <EntryRow
                      row={r}
                      selected={selected === r.entry.path}
                      active={activePath === r.entry.path}
                      git={r.entry.git ?? gitIdx.files.get(r.entry.path) ?? null}
                      dirty={r.entry.isDir && gitIdx.dirs.has(r.entry.path)}
                      pinned={!r.entry.isDir && isPinned(pins, r.entry.path)}
                      onActivate={activate}
                      onToggle={toggle}
                      onMenu={showMenu}
                    />
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>
      {menu.element}
    </div>
  );
});

export default FileTree;

function NoteRow({ row, onRetry }: { row: Extract<Row, { type: "note" }>; onRetry: (dir: string) => void }) {
  return (
    <div className="flex h-full items-center gap-1.5 pr-2 text-[12px]" style={{ paddingLeft: 8 + row.depth * INDENT + 16 }} title={row.text}>
      {row.error && <AlertTriangle className="size-3 shrink-0 text-danger" />}
      <span className={clsx("min-w-0 flex-1 truncate italic", row.error ? "text-danger/90" : "text-faint")}>{row.text}</span>
      {row.retry && <button className="shrink-0 text-[11px] text-accent hover:underline" onClick={() => onRetry(row.retry!)}>Retry</button>}
    </div>
  );
}

/** The workspace's pinned files, above the tree: click opens one again. */
function PinnedList({ pins, cwd, activePath, onMenu }: { pins: { path: string; root: string | null }[]; cwd: string; activePath: string | null; onMenu: (ev: MouseEvent, e: FileEntry) => void }) {
  return (
    <div className="max-h-[35%] shrink-0 overflow-y-auto border-b border-line pb-1">
      <div className="flex h-5 items-center gap-1 px-2.5 text-[10.5px] font-semibold tracking-wider text-faint uppercase"><Pin className="size-2.5" />Pinned</div>
      {pins.map((p) => {
        const name = basename(p.path);
        const dir = relPath(dirname(p.path), cwd);
        const entry: FileEntry = { name, path: p.path, isDir: false, size: 0, ignored: false };
        return (
          <div key={p.path} title={p.path}
            className={clsx("no-select group flex h-[22px] cursor-default items-center gap-1.5 pr-1 pl-3 text-[13px]", activePath === p.path ? "bg-hover" : "hover:bg-hover/60")}
            onClick={() => openFilePreview(p.path, p.root ?? cwd)}
            onContextMenu={(ev) => onMenu(ev, entry)}>
            <FileIcon name={name} isDir={false} />
            <span className="shrink-0 truncate text-fg/90">{name}</span>
            {dir && dir !== "." && <span className="min-w-0 flex-1 truncate text-[11px] text-faint">{dir}</span>}
            <button className="ml-auto shrink-0 text-faint opacity-0 group-hover:opacity-100 hover:text-fg" title="Unpin" aria-label={`Unpin ${name}`}
              onClick={(ev) => { ev.stopPropagation(); unpin(p.path); }}>
              <X className="size-3" />
            </button>
          </div>
        );
      })}
    </div>
  );
}

const EntryRow = memo(function EntryRow({ row, selected, active, git, dirty, pinned, onActivate, onToggle, onMenu }: {
  row: Extract<Row, { type: "entry" }>;
  selected: boolean;
  active: boolean;
  git: string | null;
  dirty: boolean;
  pinned: boolean;
  onActivate: (e: FileEntry) => void;
  onToggle: (dir: string) => void;
  onMenu: (ev: MouseEvent, e: FileEntry) => void;
}) {
  const e = row.entry;
  const tone = gitTone(git);
  return (
    <div
      className={clsx(
        "no-select group flex h-full cursor-default items-center gap-1 pr-2 text-[13px]",
        selected ? "bg-accent/20" : active ? "bg-hover" : "hover:bg-hover/60",
        e.ignored && "opacity-55",
      )}
      style={{ paddingLeft: 8 + row.depth * INDENT }}
      title={e.path}
      onClick={() => onActivate(e)}
      onContextMenu={(ev) => onMenu(ev, e)}
      // drag onto a DevCat Agent chat to attach it (local files only)
      draggable={!isRemote(e.path)}
      onDragStart={(ev) => {
        ev.dataTransfer.setData(DRAG_PATHS, JSON.stringify([e.path]));
        ev.dataTransfer.setData("text/plain", e.path);
        ev.dataTransfer.effectAllowed = "copy";
      }}
    >
      {e.isDir ? (
        <span
          className="flex size-4 shrink-0 items-center justify-center text-faint hover:text-fg"
          onClick={(ev) => { ev.stopPropagation(); onToggle(e.path); }}
        >
          {row.loading
            ? <Spin icon={Loader2} className="size-3" />
            : <ChevronRight className={clsx("size-3 transition-transform duration-100", row.expanded && "rotate-90")} />}
        </span>
      ) : <span className="size-4 shrink-0" />}
      <FileIcon name={e.name} isDir={e.isDir} open={row.expanded} />
      <span className={clsx("min-w-0 flex-1 truncate pl-0.5", tone ?? (e.isDir ? "text-fg/85" : "text-fg/90"), git?.includes("D") && "line-through")}>{e.name}</span>
      {pinned && <span className="shrink-0 text-faint" title="Pinned"><Pin className="size-3" /></span>}
      {e.isRepo && <span className="shrink-0 text-accent/70" title="Git repository — right-click to open it"><GitBranch className="size-3" /></span>}
      {e.isDir
        ? dirty && <span className="size-1.5 shrink-0 rounded-full bg-warn/80" title="Contains changes" />
        : git && <span className={clsx("w-4 shrink-0 text-right font-mono text-[10px] font-semibold", tone ?? "text-muted")}>{git.trim() === "??" ? "U" : git.trim()}</span>}
    </div>
  );
});
