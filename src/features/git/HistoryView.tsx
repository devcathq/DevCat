// History: virtualized commit list with a lane graph (all branches or current), search / author /
// file filters and paging; the selected commit's detail + diffs below.
import { useVirtualizer } from "@tanstack/react-virtual";
import clsx from "clsx";
import { FileClock, GitCommitHorizontal, RefreshCw, Search, User, X } from "lucide-react";
import { memo, useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent, type MouseEvent, type ReactNode } from "react";
import { Group, Panel, Separator } from "react-resizable-panels";
import { Empty, ErrorBox, IconButton, Spinner, Spin } from "../../components/ui";
import { api, errMsg } from "../../lib/api";
import type { GitCommit, GitRefs } from "../../lib/types";
import { useTabs } from "../../store/tabs";
import { useContextMenu } from "../connections/ContextMenu";
import { relPath } from "../files/format";
import { commitMenu, type Dialogs } from "./actions";
import { CommitDetail, RefBadge } from "./CommitDetail";
import { computeGraph, GRAPH_COLORS, type GraphRow } from "./graph";
import { useRepo } from "./store";
import { fmtUnix, relUnix } from "./util";

const PAGE = 300;
/** Stop auto-paging when looking for a commit after this many. */
const SEEK_LIMIT = 6000;
const ROW_H = 24;
const LANE_W = 14;
const MAX_LANES = 16;
const ALL_KEY = "devcat.git.allBranches";

function useDebounced<T>(value: T, ms: number): T {
  const [v, setV] = useState(value);
  useEffect(() => {
    const t = setTimeout(() => setV(value), ms);
    return () => clearTimeout(t);
  }, [value, ms]);
  return v;
}

/** Changes whenever a branch, tag or HEAD moves — the list reloads then. */
function refsSignature(refs: GitRefs | null): string {
  if (!refs) return "";
  return [refs.headSha, ...refs.local.map((b) => b.name + b.sha), ...refs.remote.map((b) => b.name + b.sha), ...refs.tags.map((t) => t.name + t.sha)].join("|");
}

export function HistoryView({ tabId, repo, path, target, dlg }: {
  tabId: string;
  repo: string;
  /** File-history filter. */
  path: string | null;
  /** Commit to select (from tab.sha); consumed. */
  target: string | null;
  dlg: Dialogs;
}) {
  const { refs, refsError } = useRepo(repo);
  const [all, setAll] = useState(() => localStorage.getItem(ALL_KEY) !== "0");
  const [searchInput, setSearchInput] = useState("");
  const [authorInput, setAuthorInput] = useState("");
  const search = useDebounced(searchInput.trim(), 350);
  const author = useDebounced(authorInput.trim(), 350);
  const [commits, setCommits] = useState<GitCommit[]>([]);
  const [hasMore, setHasMore] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [seek, setSeek] = useState(0);
  const pending = useRef<string | null>(null);
  const gen = useRef(0);
  const busy = useRef(false);
  const count = useRef(0);
  count.current = commits.length;
  const scrollRef = useRef<HTMLDivElement>(null);
  const menu = useContextMenu();

  const ready = refs !== null || refsError !== null;
  const sig = refsSignature(refs);

  const fetchPage = useCallback(async (reset: boolean) => {
    if (!reset && busy.current) return;
    const g = reset ? ++gen.current : gen.current;
    busy.current = true;
    setLoading(true);
    try {
      const list = await api.gitLog(repo, {
        all, search: search || null, author: author || null, path: path || null, skip: reset ? 0 : count.current, limit: PAGE,
      });
      if (g !== gen.current) return;
      setCommits((prev) => (reset ? list : [...prev, ...list]));
      setHasMore(list.length >= PAGE);
      setError(null);
    } catch (e) {
      if (g === gen.current) setError(errMsg(e));
    } finally {
      if (g === gen.current) {
        busy.current = false;
        setLoading(false);
      }
    }
  }, [repo, all, search, author, path]);

  useEffect(() => {
    if (ready) void fetchPage(true);
  }, [fetchPage, ready, sig]);

  const filtered = !!(search || author || path);
  // Parents are only contiguous in an unfiltered log; skip the graph otherwise.
  const graph = useMemo(() => (filtered ? null : computeGraph(commits)), [commits, filtered]);
  // At least wide enough for the "Graph" header label.
  const graphW = graph ? Math.max(Math.min(Math.max(graph.maxWidth, 1), MAX_LANES) * LANE_W + 6, 56) : 0;

  const virt = useVirtualizer({ count: commits.length, getScrollElement: () => scrollRef.current, estimateSize: () => ROW_H, overscan: 20 });
  const items = virt.getVirtualItems();
  const lastIndex = items.length ? items[items.length - 1].index : -1;

  // Page in more near the end.
  useEffect(() => {
    if (hasMore && !loading && lastIndex >= commits.length - 40) void fetchPage(false);
  }, [lastIndex, hasMore, loading, commits.length, fetchPage]);

  const goTo = useCallback((sha: string) => {
    pending.current = sha;
    setSelected(sha);
    setSeek((n) => n + 1);
  }, []);

  // Commit requested by the tab (blame, branches view, …).
  useEffect(() => {
    if (!target) return;
    goTo(target);
    useTabs.getState().update(tabId, { sha: null } as never);
  }, [target, tabId, goTo]);

  // Scroll to the pending commit once it's loaded, paging further if needed.
  useEffect(() => {
    const t = pending.current;
    if (!t || !ready) return;
    const i = commits.findIndex((c) => c.sha.startsWith(t));
    if (i >= 0) {
      pending.current = null;
      setSelected(commits[i].sha);
      virt.scrollToIndex(i, { align: "center" });
    } else if (loading) {
      return;
    } else if (hasMore && commits.length < SEEK_LIMIT) {
      void fetchPage(false);
    } else {
      pending.current = null;
    }
  }, [commits, hasMore, loading, seek, ready, fetchPage, virt]);

  const setAllBranches = (v: boolean) => {
    localStorage.setItem(ALL_KEY, v ? "1" : "0");
    setAll(v);
  };

  const selIndex = selected ? commits.findIndex((c) => c.sha === selected) : -1;
  const onKey = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    e.preventDefault();
    const i = Math.max(0, Math.min(commits.length - 1, selIndex + (e.key === "ArrowDown" ? 1 : -1)));
    if (commits[i]) {
      setSelected(commits[i].sha);
      virt.scrollToIndex(i);
    }
  };

  const showCtx = menu.show;
  const showMenu = useCallback((e: MouseEvent, c: GitCommit) => {
    setSelected(c.sha);
    showCtx(e, commitMenu(repo, dlg, c, refs));
  }, [showCtx, repo, dlg, refs]);

  const setPath = (p: string | null) => useTabs.getState().update(tabId, { path: p } as never);
  const headSha = refs?.headSha ?? null;

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex h-9 shrink-0 items-center gap-1.5 border-b border-line bg-panel px-2">
        <div className="flex h-6 items-center rounded-md border border-line bg-app p-px text-[12px]">
          {([[true, "All branches"], [false, "Current"]] as const).map(([v, label]) => (
            <button
              key={label}
              onClick={() => setAllBranches(v)}
              className={clsx("h-full rounded px-2", all === v ? "bg-hover text-fg" : "text-muted hover:text-fg")}
            >
              {label}
            </button>
          ))}
        </div>
        <FilterInput icon={<Search className="size-3" />} value={searchInput} onChange={setSearchInput} placeholder="Message or SHA" className="w-56" />
        <FilterInput icon={<User className="size-3" />} value={authorInput} onChange={setAuthorInput} placeholder="Author" className="w-36" />
        {path && (
          <span className="inline-flex h-6 min-w-0 items-center gap-1 rounded-md border border-accent/40 bg-accent/10 pr-1 pl-2 text-[12px] text-accent" title={path}>
            <FileClock className="size-3 shrink-0" />
            <span className="truncate font-mono">{relPath(path, repo)}</span>
            <button className="shrink-0 rounded p-0.5 hover:bg-accent/20" title="Show all files" onClick={() => setPath(null)}><X className="size-3" /></button>
          </span>
        )}
        <div className="flex-1" />
        {filtered && commits.length > 0 && <span className="shrink-0 text-[11px] text-faint">graph hidden while filtering</span>}
        <span className="shrink-0 text-[11px] text-faint">{commits.length}{hasMore ? "+" : ""} commit{commits.length === 1 ? "" : "s"}</span>
        <IconButton title="Reload" onClick={() => void fetchPage(true)}><Spin icon={RefreshCw} spinning={loading} className="size-3.5" /></IconButton>
      </div>
      <Group orientation="vertical" className="min-h-0 flex-1">
        <Panel defaultSize="52%" minSize="15%">
          <div className="flex h-full min-h-0 flex-col">
            <div className="flex h-6 shrink-0 items-center border-b border-line bg-panel pr-3 text-[10.5px] font-semibold tracking-wide text-faint uppercase">
              {graph && <div className="shrink-0 pl-2" style={{ width: graphW }}>Graph</div>}
              <div className={clsx("min-w-0 flex-1", !graph && "pl-3")}>Description</div>
              <div className="w-36 shrink-0">Author</div>
              <div className="w-24 shrink-0">Date</div>
              <div className="w-16 shrink-0">Commit</div>
            </div>
            {error ? <ErrorBox error={error} /> : !commits.length ? (
              loading || !ready
                ? <div className="flex flex-1 items-center justify-center"><Spinner /></div>
                : <Empty icon={<GitCommitHorizontal className="size-8" />} title={filtered ? "No matching commits" : "No commits yet"} />
            ) : (
              <div ref={scrollRef} tabIndex={0} onKeyDown={onKey} className="min-h-0 flex-1 overflow-auto outline-none">
                <div style={{ height: virt.getTotalSize(), position: "relative" }}>
                  {items.map((vi) => {
                    const c = commits[vi.index];
                    return (
                      <div key={c.sha} style={{ position: "absolute", top: 0, left: 0, right: 0, height: ROW_H, transform: `translateY(${vi.start}px)` }}>
                        <CommitRow
                          c={c}
                          graph={graph?.rows[vi.index] ?? null}
                          graphW={graphW}
                          head={c.sha === headSha}
                          selected={c.sha === selected}
                          onSelect={setSelected}
                          onMenu={showMenu}
                        />
                      </div>
                    );
                  })}
                </div>
                {loading && <div className="flex h-6 items-center justify-center text-[11px] text-faint"><Spinner className="mr-1.5 size-3" />Loading…</div>}
              </div>
            )}
          </div>
        </Panel>
        <Separator className="h-px bg-line hover:bg-accent data-[separator=active]:bg-accent" />
        <Panel minSize="20%">
          {selected ? (
            <CommitDetail
              repo={repo}
              sha={selected}
              onNavigate={goTo}
              onFileHistory={(p) => setPath(p)}
              onMenu={(e, d) => menu.show(e, commitMenu(repo, dlg, d.commit, refs))}
            />
          ) : (
            <Empty icon={<GitCommitHorizontal className="size-8" />} title="No commit selected">Select a commit to see its changes. Right-click for actions.</Empty>
          )}
        </Panel>
      </Group>
      {menu.element}
    </div>
  );
}

function FilterInput({ icon, value, onChange, placeholder, className }: {
  icon: ReactNode; value: string; onChange: (v: string) => void; placeholder: string; className?: string;
}) {
  return (
    <div className={clsx("relative shrink-0", className)}>
      <span className="pointer-events-none absolute top-1/2 left-1.5 -translate-y-1/2 text-faint">{icon}</span>
      <input
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => e.key === "Escape" && onChange("")}
        placeholder={placeholder}
        spellCheck={false}
        className="h-6 w-full rounded border border-line bg-app pr-5 pl-5.5 text-[12px] placeholder:text-faint focus:border-accent focus:outline-none"
      />
      {value && (
        <button className="absolute top-1/2 right-1 -translate-y-1/2 text-faint hover:text-fg" onClick={() => onChange("")} aria-label="Clear">
          <X className="size-3" />
        </button>
      )}
    </div>
  );
}

const CommitRow = memo(function CommitRow({ c, graph, graphW, head, selected, onSelect, onMenu }: {
  c: GitCommit;
  graph: GraphRow | null;
  graphW: number;
  head: boolean;
  selected: boolean;
  onSelect: (sha: string) => void;
  onMenu: (e: MouseEvent, c: GitCommit) => void;
}) {
  const shown = c.refs.slice(0, 4);
  return (
    <div
      className={clsx("flex h-full cursor-default items-center pr-3 text-[12px]", selected ? "bg-accent/15" : "hover:bg-hover/60")}
      onClick={() => onSelect(c.sha)}
      onContextMenu={(e) => onMenu(e, c)}
    >
      {graph && <GraphCell row={graph} width={graphW} head={head} merge={c.parents.length > 1} />}
      <div className={clsx("flex min-w-0 flex-1 items-center gap-1 pr-2", !graph && "pl-3")}>
        {shown.map((r) => <RefBadge key={r.kind + r.name} name={r.name} kind={r.kind} />)}
        {c.refs.length > shown.length && (
          <span className="shrink-0 text-[10px] text-faint" title={c.refs.slice(shown.length).map((r) => r.name).join(", ")}>+{c.refs.length - shown.length}</span>
        )}
        <span className={clsx("truncate", head ? "font-semibold text-fg" : c.parents.length > 1 ? "text-muted" : "text-fg/90")} title={c.subject}>{c.subject}</span>
      </div>
      <div className="w-36 shrink-0 truncate pr-2 text-muted" title={c.email}>{c.author}</div>
      <div className="w-24 shrink-0 truncate pr-2 text-muted" title={fmtUnix(c.time)}>{relUnix(c.time)}</div>
      <div className="w-16 shrink-0 font-mono text-[11px] text-faint">{c.short}</div>
    </div>
  );
});

function GraphCell({ row, width, head, merge }: { row: GraphRow; width: number; head: boolean; merge: boolean }) {
  const x = (lane: number) => lane * LANE_W + LANE_W / 2 + 3;
  const mid = ROW_H / 2;
  const color = (i: number) => GRAPH_COLORS[i % GRAPH_COLORS.length];
  const nodeColor = color(row.color);
  return (
    <svg width={width} height={ROW_H} className="shrink-0">
      {row.edges.map((e, i) => {
        const stroke = color(e.color);
        if (e.half === "full") return <line key={i} x1={x(e.from)} y1={0} x2={x(e.from)} y2={ROW_H} stroke={stroke} strokeWidth={1.6} />;
        const [y1, y2] = e.half === "top" ? [0, mid] : [mid, ROW_H];
        const x1 = x(e.from), x2 = x(e.to);
        if (x1 === x2) return <line key={i} x1={x1} y1={y1} x2={x2} y2={y2} stroke={stroke} strokeWidth={1.6} />;
        const ym = (y1 + y2) / 2;
        return <path key={i} d={`M${x1} ${y1} C${x1} ${ym} ${x2} ${ym} ${x2} ${y2}`} fill="none" stroke={stroke} strokeWidth={1.6} />;
      })}
      {head && <circle cx={x(row.col)} cy={mid} r={6.5} fill="none" stroke={nodeColor} strokeWidth={1.2} />}
      <circle
        cx={x(row.col)}
        cy={mid}
        r={merge ? 3 : 4}
        fill={merge ? "var(--color-panel)" : nodeColor}
        stroke={nodeColor}
        strokeWidth={merge ? 1.6 : 0}
      />
    </svg>
  );
}
