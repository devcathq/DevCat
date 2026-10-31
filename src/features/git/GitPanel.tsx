// Git sidebar: scans a folder for repositories (nested ones too) and shows them as a tree
// with branch, ahead/behind and change count. Click opens the repo's Git tab.
import clsx from "clsx";
import {
  AlertTriangle, ArrowDown, ArrowDownToLine, ArrowUp, ArrowUpFromLine, Boxes, ChevronDown, ChevronRight, Copy, FolderGit2, FolderOpen, FolderSearch,
  GitBranch, Layers, Loader2, RefreshCw, Search, SquareArrowOutUpRight, X,
} from "lucide-react";
import { AiTerminalIcon } from "../../components/ui/AppIcons";
import { useCallback, useEffect, useMemo, useRef, useState, type MouseEvent } from "react";
import { Button, ErrorBox, IconButton, Spin } from "../../components/ui";
import { api, errMsg } from "../../lib/api";
import type { GitRepo } from "../../lib/types";
import { useTabs } from "../../store/tabs";
import { useUi } from "../../store/ui";
import { openClaudeWorkspace } from "../claude/open";
import { useWorkspace } from "../claude/sidebar/workspace";
import { useContextMenu, type MenuItem } from "../connections/ContextMenu";
import { basename, copyText, revealPath, REVEAL_LABEL } from "../files/format";
import { openGitRepo } from "./open";
import { DEFAULT_DEPTH, pickScanRoot, useGitScan, useScanRoot } from "./scan";
import { gitAction, useGitRepos } from "./store";
import { tildify } from "./util";

const INDENT = 12;
/** Rescan on window focus at most this often. */
const FOCUS_RESCAN_MS = 10_000;

type Row = { repo: GitRepo; depth: number; hasChildren: boolean };

/** Nest repos by `parent`; enclosing repos ("..") sort first, then by relative path. */
function buildRows(repos: GitRepo[], collapsed: Set<string>, q: string): Row[] {
  const byPath = new Map(repos.map((r) => [r.path, r]));
  const kids = new Map<string | null, GitRepo[]>();
  for (const r of repos) {
    const p = r.parent && byPath.has(r.parent) ? r.parent : null;
    kids.set(p, [...(kids.get(p) ?? []), r]);
  }
  const order = (a: GitRepo, b: GitRepo) => {
    const ua = a.rel.startsWith(".."), ub = b.rel.startsWith("..");
    if (ua !== ub) return ua ? -1 : 1;
    return a.rel.localeCompare(b.rel, undefined, { numeric: true, sensitivity: "base" });
  };
  const match = (r: GitRepo) => !q || [r.name, r.rel, r.branch ?? ""].some((s) => s.toLowerCase().includes(q));
  const out: Row[] = [];
  // Filtering keeps matches + their ancestors and ignores collapsing.
  const walk = (parent: string | null, depth: number, into: Row[]): boolean => {
    let any = false;
    for (const r of [...(kids.get(parent) ?? [])].sort(order)) {
      const children = kids.get(r.path) ?? [];
      const row = { repo: r, depth, hasChildren: children.length > 0 };
      const sub: Row[] = [];
      const childHit = children.length > 0 && (q || !collapsed.has(r.path)) ? walk(r.path, depth + 1, sub) : false;
      if (!q || childHit || match(r)) {
        into.push(row, ...sub);
        any = true;
      }
    }
    return any;
  };
  walk(null, 0, out);
  return out;
}

export default function GitPanel() {
  const root = useScanRoot();
  return (
    <div className="flex h-full flex-col">
      <RootHeader root={root} />
      {root ? <RepoList key={root} root={root} /> : <NoRoot />}
    </div>
  );
}

function NoRoot() {
  return (
    <div className="flex flex-1 flex-col items-center justify-center gap-3 p-6 text-center">
      <FolderGit2 className="size-8 text-accent/70" />
      <div className="font-medium text-fg/85">No folder selected</div>
      <div className="max-w-56 text-xs text-muted">
        Pick a folder to find every git repository inside it — including nested repos and submodules.
      </div>
      <Button variant="primary" onClick={() => void pickScanRoot(null)}><FolderOpen className="size-3.5" /> Choose folder…</Button>
    </div>
  );
}

/** Scan folder + picker menu (choose…, Claude workspace folder, recents, depth). */
function RootHeader({ root }: { root: string | null }) {
  const chosen = useGitScan((s) => s.root);
  const recent = useGitScan((s) => s.recent);
  const depth = useGitScan((s) => s.depth);
  const setRoot = useGitScan((s) => s.setRoot);
  const forgetRecent = useGitScan((s) => s.forgetRecent);
  const setDepth = useGitScan((s) => s.setDepth);
  const claudeCwd = useWorkspace((s) => s.cwd);
  const menu = useContextMenu();

  const showMenu = (e: MouseEvent) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const items: MenuItem[] = [
      { label: "Choose folder…", icon: <FolderOpen className="size-3.5" />, onClick: () => void pickScanRoot(root) },
      {
        label: claudeCwd ? `Follow AI workspace (${basename(claudeCwd)})` : "Follow AI workspace",
        icon: <AiTerminalIcon className="size-3.5" />,
        disabled: !claudeCwd || chosen === null,
        onClick: () => setRoot(null),
      },
      {
        label: `Scan depth: ${depth}`,
        icon: <Layers className="size-3.5" />,
        submenu: [2, 4, DEFAULT_DEPTH, 8, 12].map((d): MenuItem => ({
          label: `${d} levels${d === DEFAULT_DEPTH ? " (default)" : ""}`, disabled: d === depth, onClick: () => setDepth(d),
        })),
      },
    ];
    const others = recent.filter((x) => x !== root);
    if (others.length) {
      items.push("separator");
      for (const x of others) items.push({ label: tildify(x), icon: <FolderOpen className="size-3.5" />, onClick: () => setRoot(x) });
      items.push({ label: "Clear recent folders", icon: <X className="size-3.5" />, onClick: () => others.forEach(forgetRecent) });
    }
    if (root) {
      items.push(
        "separator",
        { label: "Copy folder path", icon: <Copy className="size-3.5" />, onClick: () => void copyText(root) },
        { label: REVEAL_LABEL, icon: <FolderSearch className="size-3.5" />, onClick: () => void revealPath(root) },
      );
    }
    menu.show({ clientX: r.left, clientY: r.bottom + 2 }, items);
  };

  return (
    <div className="shrink-0 border-b border-line px-2 py-1.5">
      <button
        className="flex w-full items-center gap-1.5 rounded px-1.5 py-1 text-left hover:bg-hover"
        title={root ?? "Choose a folder"}
        onClick={showMenu}
      >
        <FolderGit2 className={clsx("size-4 shrink-0", root ? "text-accent" : "text-faint")} />
        <div className="min-w-0 flex-1">
          <div className={clsx("truncate text-[13px] font-medium", !root && "text-muted")}>{root ? basename(root) : "Choose folder…"}</div>
          {root && (
            <div className="truncate text-[11px] text-faint">
              {tildify(root)}{chosen === null && " · AI workspace"}
            </div>
          )}
        </div>
        <ChevronDown className="size-3.5 shrink-0 text-faint" />
      </button>
      {menu.element}
    </div>
  );
}

function RepoList({ root }: { root: string }) {
  const depth = useGitScan((s) => s.depth);
  const [repos, setRepos] = useState<GitRepo[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [filter, setFilter] = useState("");
  const [collapsed, setCollapsed] = useState<Set<string>>(() => new Set());
  const [busy, setBusy] = useState<Set<string>>(() => new Set());
  const live = useGitRepos((s) => s.repos);
  const activeRepo = useTabs((s) => {
    const t = s.tabs.find((x) => x.id === s.activeId);
    return t?.type === "git" ? t.repo : null;
  });
  const menu = useContextMenu();
  const lastScan = useRef(0);

  const scan = useCallback(async (quiet = false) => {
    if (!quiet) setLoading(true);
    lastScan.current = Date.now();
    try {
      setRepos(await api.gitRepos(root, depth));
      setError(null);
    } catch (e) {
      if (!quiet) setError(errMsg(e));
    } finally {
      setLoading(false);
    }
  }, [root, depth]);

  useEffect(() => { void scan(); }, [scan]);

  // Coming back to the app (e.g. after committing in a terminal): rescan, debounced.
  useEffect(() => {
    const onFocus = () => { if (Date.now() - lastScan.current > FOCUS_RESCAN_MS) void scan(true); };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [scan]);

  // Status read by an open Git tab is fresher than the scan.
  const merged = useMemo(() => (repos ?? []).map((r): GitRepo => {
    const st = live[r.path]?.status;
    if (!st?.isRepo) return r;
    return { ...r, branch: st.branch ?? r.branch, ahead: st.ahead, behind: st.behind, changes: st.files.length };
  }), [repos, live]);

  const q = filter.trim().toLowerCase();
  const rows = useMemo(() => buildRows(merged, collapsed, q), [merged, collapsed, q]);
  const dirty = merged.filter((r) => r.changes > 0).length;

  const toggle = (path: string) => setCollapsed((s) => {
    const n = new Set(s);
    if (n.has(path)) n.delete(path); else n.add(path);
    return n;
  });

  const remote = async (r: GitRepo, op: "fetch" | "pull" | "push") => {
    setBusy((s) => new Set(s).add(r.path));
    const label = { fetch: "Fetch", pull: "Pull", push: "Push" }[op] + ` ${r.name}`;
    await gitAction(r.path, label, () => (op === "fetch" ? api.gitFetch(r.path) : op === "pull" ? api.gitPull(r.path) : api.gitPush(r.path)));
    setBusy((s) => { const n = new Set(s); n.delete(r.path); return n; });
    void scan(true);
  };

  const showMenu = (e: MouseEvent, r: GitRepo) => {
    const items: MenuItem[] = [
      { label: "Open", icon: <SquareArrowOutUpRight className="size-3.5" />, onClick: () => openGitRepo(r.path) },
      { label: "Show changes", icon: <GitBranch className="size-3.5" />, onClick: () => openGitRepo(r.path, { view: "changes" }) },
      "separator",
      { label: "Fetch", icon: <RefreshCw className="size-3.5" />, disabled: busy.has(r.path), onClick: () => void remote(r, "fetch") },
      { label: "Pull", icon: <ArrowDownToLine className="size-3.5" />, disabled: busy.has(r.path), onClick: () => void remote(r, "pull") },
      { label: "Push", icon: <ArrowUpFromLine className="size-3.5" />, disabled: busy.has(r.path) || !r.upstream, onClick: () => void remote(r, "push") },
      "separator",
      {
        label: "Open in AI workspace", icon: <AiTerminalIcon className="size-3.5" />, onClick: () => {
          useWorkspace.getState().setCwd(r.path);
          useUi.getState().setSidebar("claude");
        },
      },
      { label: "New AI chat here", icon: <AiTerminalIcon className="size-3.5" />, onClick: () => openClaudeWorkspace(r.path) },
      { label: "Scan from this folder", icon: <FolderSearch className="size-3.5" />, onClick: () => useGitScan.getState().setRoot(r.path) },
      "separator",
      { label: "Copy path", icon: <Copy className="size-3.5" />, onClick: () => void copyText(r.path) },
      { label: REVEAL_LABEL, icon: <FolderSearch className="size-3.5" />, onClick: () => void revealPath(r.path) },
    ];
    menu.show(e, items);
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex h-8 shrink-0 items-center gap-1 px-2">
        <div className="relative min-w-0 flex-1">
          <Search className="pointer-events-none absolute top-1/2 left-1.5 size-3 -translate-y-1/2 text-faint" />
          <input
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            onKeyDown={(e) => e.key === "Escape" && setFilter("")}
            placeholder="Filter repositories"
            spellCheck={false}
            className="h-5.5 w-full rounded border border-line bg-app pr-5 pl-6 text-[12px] placeholder:text-faint focus:border-accent focus:outline-none"
          />
          {filter && (
            <button className="absolute top-1/2 right-1 -translate-y-1/2 text-faint hover:text-fg" onClick={() => setFilter("")} aria-label="Clear filter">
              <X className="size-3" />
            </button>
          )}
        </div>
        <IconButton title="Rescan" onClick={() => void scan()} className="size-5.5">
          <Spin icon={RefreshCw} spinning={loading} className="size-3.5" />
        </IconButton>
      </div>
      {repos && (
        <div className="shrink-0 px-3 pb-1 text-[11px] text-faint">
          {repos.length} repositor{repos.length === 1 ? "y" : "ies"}{dirty > 0 && ` · ${dirty} with changes`}
        </div>
      )}
      <div className="min-h-0 flex-1 overflow-y-auto pb-2">
        {error ? (
          <ErrorBox error={error} />
        ) : !repos ? (
          <div className="flex items-center gap-2 px-3 py-2 text-[12px] text-faint"><Spin icon={Loader2} className="size-3.5" />Scanning…</div>
        ) : repos.length === 0 ? (
          <div className="px-3 py-4 text-center text-[12px] text-muted">
            No git repositories found within {depth} levels of {basename(root)}.
          </div>
        ) : rows.length === 0 ? (
          <div className="px-3 py-2 text-[12px] text-faint italic">No repository matches “{filter}”.</div>
        ) : rows.map((row) => (
          <RepoRow
            key={row.repo.path}
            row={row}
            active={activeRepo === row.repo.path}
            collapsed={collapsed.has(row.repo.path) && !q}
            busy={busy.has(row.repo.path)}
            onToggle={() => toggle(row.repo.path)}
            onMenu={(e) => showMenu(e, row.repo)}
          />
        ))}
      </div>
      {menu.element}
    </div>
  );
}

function RepoRow({ row, active, collapsed, busy, onToggle, onMenu }: {
  row: Row; active: boolean; collapsed: boolean; busy: boolean; onToggle: () => void; onMenu: (e: MouseEvent) => void;
}) {
  const r = row.repo;
  const outside = r.rel.startsWith("..");
  return (
    <div
      className={clsx("group flex cursor-default items-start gap-1 py-1 pr-2", active ? "bg-accent/15" : "hover:bg-hover/60")}
      style={{ paddingLeft: 6 + row.depth * INDENT }}
      title={r.error ? `${r.path}\n${r.error}` : r.path}
      onClick={() => openGitRepo(r.path)}
      onContextMenu={onMenu}
    >
      {row.hasChildren ? (
        <span className="mt-px flex size-4 shrink-0 items-center justify-center text-faint hover:text-fg" onClick={(e) => { e.stopPropagation(); onToggle(); }}>
          <ChevronRight className={clsx("size-3 transition-transform duration-100", !collapsed && "rotate-90")} />
        </span>
      ) : <span className="size-4 shrink-0" />}
      {r.isSubmodule
        ? <Boxes className="mt-0.5 size-3.5 shrink-0 text-info" />
        : <FolderGit2 className={clsx("mt-0.5 size-3.5 shrink-0", r.error ? "text-danger" : "text-accent/80")} />}
      <div className="min-w-0 flex-1 pl-0.5">
        <div className="flex items-center gap-1.5">
          <span className="min-w-0 truncate text-[13px] text-fg/90">{r.name}</span>
          {r.isSubmodule && <span className="shrink-0 rounded border border-info/30 px-1 text-[9px] font-semibold tracking-wide text-info uppercase" title="Submodule or linked worktree">sub</span>}
          {outside && <span className="shrink-0 text-[10px] text-faint" title="This repository contains the scanned folder">enclosing</span>}
          <span className="flex-1" />
          {busy && <Spin icon={Loader2} className="size-3 shrink-0 text-muted" />}
          {r.error && <AlertTriangle className="size-3 shrink-0 text-danger" />}
          {r.changes > 0 && (
            <span className="shrink-0 rounded bg-warn/15 px-1 font-mono text-[10px] font-semibold text-warn" title={`${r.changes} changed file${r.changes === 1 ? "" : "s"}`}>
              {r.changes}
            </span>
          )}
        </div>
        <div className="flex items-center gap-1.5 text-[11px]">
          <span className="min-w-0 flex-1 truncate text-faint">{r.rel === "." ? "(scanned folder)" : r.rel}</span>
          {!r.error && (
            <span className="inline-flex min-w-0 max-w-[60%] shrink items-center gap-0.5 font-mono text-muted">
              <GitBranch className="size-3 shrink-0" /><span className="truncate">{r.branch ?? (r.headSha ? r.headSha.slice(0, 7) : "detached")}</span>
            </span>
          )}
          {r.ahead > 0 && <span className="inline-flex shrink-0 items-center text-info" title={`${r.ahead} ahead of ${r.upstream ?? "upstream"}`}><ArrowUp className="size-3" />{r.ahead}</span>}
          {r.behind > 0 && <span className="inline-flex shrink-0 items-center text-warn" title={`${r.behind} behind ${r.upstream ?? "upstream"}`}><ArrowDown className="size-3" />{r.behind}</span>}
        </div>
      </div>
    </div>
  );
}
