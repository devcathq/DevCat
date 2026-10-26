// Lazy, virtualized directory tree for the Claude workspace folder (git-status aware).
import { useVirtualizer } from "@tanstack/react-virtual";
import clsx from "clsx";
import {
  AlertTriangle, AtSign, ChevronRight, ChevronsDownUp, ClipboardCopy, ClipboardPaste, Copy, Eye, Files, EyeOff, FolderGit2, FolderSearch, GitBranch, History, Loader2, RefreshCw, Search,
  SquareArrowOutUpRight, UserSearch, X,
} from "lucide-react";
import { forwardRef, memo, useCallback, useEffect, useImperativeHandle, useMemo, useRef, useState, type KeyboardEvent, type MouseEvent } from "react";
import { toast } from "sonner";
import { IconButton } from "../../../components/ui";
import { api } from "../../../lib/api";
import type { FileEntry, GitStatus } from "../../../lib/types";
import { useTabs } from "../../../store/tabs";
import { useContextMenu, type MenuItem } from "../../connections/ContextMenu";
import { FileIcon } from "../../files/FileIcon";
import { basename, copyFile, copyText, dirname, openFilePreview, relPath, revealPath, REVEAL_LABEL } from "../../files/format";
import { openBlame, openGitRepo } from "../../git/open";
import { repoRootOf } from "../../git/repoOf";
import { useWorkspace } from "./workspace";
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
  const activePath = useTabs((s) => {
    const t = s.tabs.find((x) => x.id === s.activeId);
    return t?.type === "file" ? t.path : null;
  });
  const gitIdx = useGitIndex(git);
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
  useEffect(() => { setExpanded(new Set()); setSelected(null); setFilter(""); }, [cwd]);

  // Expanded dirs without a listing (after a reset) get loaded.
  useEffect(() => {
    for (const d of expanded) if (!listings[d]) void load(d);
  }, [expanded, listings, load]);

  const refresh = useCallback(() => {
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
    const q = filter.trim().toLowerCase();
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
  }, [cwd, listings, expanded, filter]);

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

  const activate = useCallback((e: FileEntry) => {
    setSelected(e.path);
    if (e.isDir) toggle(e.path);
    else openFilePreview(e.path, cwd);
  }, [cwd, toggle]);

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
    ];
    menu.show(ev, items);
  }, [cwd, expanded, toggle, menu, paste]);

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
            placeholder="Filter loaded files"
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
          <RefreshCw className={clsx("size-3.5", root?.state === "loading" && "animate-spin")} />
        </IconButton>
      </div>
      <div
        ref={scrollRef}
        tabIndex={0}
        onKeyDown={onKey}
        className="min-h-0 flex-1 overflow-auto outline-none focus-visible:outline-none"
      >
        {filter && rows.length === 0 ? (
          <div className="px-3 py-2 text-[12px] text-faint italic">No loaded file matches “{filter}”. Expand folders to search deeper.</div>
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

const EntryRow = memo(function EntryRow({ row, selected, active, git, dirty, onActivate, onToggle, onMenu }: {
  row: Extract<Row, { type: "entry" }>;
  selected: boolean;
  active: boolean;
  git: string | null;
  dirty: boolean;
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
            ? <Loader2 className="size-3 animate-spin" />
            : <ChevronRight className={clsx("size-3 transition-transform duration-100", row.expanded && "rotate-90")} />}
        </span>
      ) : <span className="size-4 shrink-0" />}
      <FileIcon name={e.name} isDir={e.isDir} open={row.expanded} />
      <span className={clsx("min-w-0 flex-1 truncate pl-0.5", tone ?? (e.isDir ? "text-fg/85" : "text-fg/90"), git?.includes("D") && "line-through")}>{e.name}</span>
      {e.isRepo && <span className="shrink-0 text-accent/70" title="Git repository — right-click to open it"><GitBranch className="size-3" /></span>}
      {e.isDir
        ? dirty && <span className="size-1.5 shrink-0 rounded-full bg-warn/80" title="Contains changes" />
        : git && <span className={clsx("w-4 shrink-0 text-right font-mono text-[10px] font-semibold", tone ?? "text-muted")}>{git.trim() === "??" ? "U" : git.trim()}</span>}
    </div>
  );
});
