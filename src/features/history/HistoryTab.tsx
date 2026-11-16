// Full-page history: searchable/filterable query log + saved queries, with a preview pane.
import MiniSelect from "../grid/MiniSelect";
import { useVirtualizer } from "@tanstack/react-virtual";
import clsx from "clsx";
import { CircleCheck, CircleX, Copy, FileCode2, History, Pencil, Play, RefreshCw, Search, Trash2 } from "lucide-react";
import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { Button, Empty, ErrorBox, IconButton, Spinner, Toolbar } from "../../components/ui";
import { KindIcon } from "../../components/ui/KindIcon";
import { DATABASE_KINDS, type HistoryEntry, type SavedQuery } from "../../lib/types";
import { inFocus, useConnections, useScopedConnections } from "../../store/connections";
import { useTabVisible } from "../../store/tabs";
import type { Tab } from "../../tabs/types";
import { useContextMenu, type MenuItem } from "../connections/ContextMenu";
import {
  absoluteTime, clearHistory, connectionName, copyText, deleteSavedQuery, firstLine, formatDuration, openHistoryEntry, openSavedQuery,
  relativeTime, renameSavedQuery,
} from "./historyActions";
import { useNow } from "./HistoryPanel";
import { useHistoryData } from "./useHistoryData";

type View = "history" | "saved";
type StatusFilter = "all" | "ok" | "error";

const ROW_H = 28;
const HISTORY_COLS = "28px 150px minmax(120px,180px) minmax(90px,140px) 80px minmax(200px,1fr)";
const SAVED_COLS = "28px minmax(160px,240px) minmax(120px,180px) minmax(90px,140px) 150px minmax(200px,1fr)";

export default function HistoryTab({ tab }: { tab: Extract<Tab, { type: "history" }> }) {
  const [view, setView] = useState<View>("history");
  const [search, setSearch] = useState("");
  const [connFilter, setConnFilter] = useState("");
  const [status, setStatus] = useState<StatusFilter>("all");
  const [selected, setSelected] = useState<string | null>(null);
  const connections = useScopedConnections();
  useConnections((s) => s.focusProjectId);
  const active = useTabVisible(tab.id);
  const { history, saved, loading, error, refresh } = useHistoryData({ search, connectionId: connFilter || null, limit: 2000 });
  const menu = useContextMenu();
  useNow(60_000);

  // Reload when the tab becomes visible again.
  const wasActive = useRef(active);
  useEffect(() => {
    if (active && !wasActive.current) refresh();
    wasActive.current = active;
  }, [active, refresh]);

  const historyRows = useMemo(
    () => history.filter((e) => inFocus(e.connectionId) && (status === "all" || (status === "ok" ? e.success : !e.success))),
    [history, status, connections],
  );
  const savedRows = useMemo(() => {
    const s = search.trim().toLowerCase();
    return saved.filter((q) =>
      inFocus(q.connectionId) &&
      (!connFilter || q.connectionId === connFilter) &&
      (!s || q.name.toLowerCase().includes(s) || q.query.toLowerCase().includes(s)));
  }, [saved, search, connFilter, connections]);

  const failed = useMemo(() => history.filter((e) => !e.success).length, [history]);
  const selHistory = view === "history" ? historyRows.find((e) => e.id === selected) : undefined;
  const selSaved = view === "saved" ? savedRows.find((q) => q.id === selected) : undefined;

  const historyMenu = (e: HistoryEntry): MenuItem[] => [
    { label: "Open in new query", icon: <Play className="size-3.5" />, onClick: () => openHistoryEntry(e) },
    { label: "Copy SQL", icon: <Copy className="size-3.5" />, onClick: () => copyText(e.query) },
    ...(e.error ? [{ label: "Copy error", icon: <Copy className="size-3.5" />, onClick: () => copyText(e.error!, "error") }] : []),
  ];
  const savedMenu = (q: SavedQuery): MenuItem[] => [
    { label: "Open", icon: <Play className="size-3.5" />, onClick: () => openSavedQuery(q) },
    { label: "Rename…", icon: <Pencil className="size-3.5" />, onClick: async () => { if (await renameSavedQuery(q)) refresh(); } },
    { label: "Copy SQL", icon: <Copy className="size-3.5" />, onClick: () => copyText(q.query) },
    "separator",
    { label: "Delete", icon: <Trash2 className="size-3.5" />, danger: true, onClick: async () => { if (await deleteSavedQuery(q)) refresh(); } },
  ];

  const dbConnections = connections.filter((c) => DATABASE_KINDS.includes(c.kind));

  return (
    <div className="flex h-full flex-col">
      <Toolbar className="gap-2">
        <div className="inline-flex h-6 rounded-md border border-line bg-app p-0.5">
          {([["history", "History", history.length], ["saved", "Saved queries", saved.length]] as const).map(([v, label, n]) => (
            <button
              key={v}
              onClick={() => { setView(v); setSelected(null); }}
              className={clsx("flex items-center gap-1.5 rounded px-2 text-xs", view === v ? "bg-raised text-fg" : "text-muted hover:text-fg")}
            >
              {label}<span className="text-faint tabular-nums">{n}</span>
            </button>
          ))}
        </div>
        <div className="relative w-64">
          <Search className="pointer-events-none absolute top-1/2 left-2 size-3.5 -translate-y-1/2 text-faint" />
          <input
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            onKeyDown={(e) => e.key === "Escape" && setSearch("")}
            placeholder={view === "history" ? "Search SQL" : "Search name or SQL"}
            spellCheck={false}
            className="h-6 w-full rounded border border-line bg-app pr-2 pl-7 text-xs text-fg placeholder:text-faint focus:border-accent focus:outline-none"
          />
        </div>
        <MiniSelect value={connFilter} onChange={(e) => setConnFilter(e.target.value)} className="w-48">
          <option value="">All connections</option>
          {dbConnections.map((c) => <option key={c.id} value={c.id}>{c.name}</option>)}
        </MiniSelect>
        {view === "history" && (
          <MiniSelect value={status} onChange={(e) => setStatus(e.target.value as StatusFilter)} className="w-32">
            <option value="all">Any status</option>
            <option value="ok">Succeeded</option>
            <option value="error">Failed{failed ? ` (${failed})` : ""}</option>
          </MiniSelect>
        )}
        <div className="flex-1" />
        <IconButton title="Refresh" onClick={refresh}>{loading ? <Spinner className="size-3.5" /> : <RefreshCw className="size-3.5" />}</IconButton>
        {view === "history" && (
          <Button size="sm" variant="danger" disabled={history.length === 0}
            onClick={async () => { if (await clearHistory()) { setSelected(null); refresh(); } }}>
            <Trash2 className="size-3" />Clear all
          </Button>
        )}
      </Toolbar>

      {error && <ErrorBox error={error} />}

      <div className="min-h-0 flex-1">
        {view === "history" ? (
          <VirtualTable
            rows={historyRows}
            cols={HISTORY_COLS}
            headers={["", "Executed", "Connection", "Database", "Duration", "Query"]}
            selected={selected}
            onSelect={setSelected}
            onOpen={openHistoryEntry}
            onContext={(ev, e) => menu.show(ev, historyMenu(e))}
            empty={loading ? null : (
              <Empty icon={<History className="size-8" />} title={search || connFilter || status !== "all" ? "No matching queries" : "No history yet"}>
                Every statement you run is recorded here with its timing and result.
              </Empty>
            )}
            renderRow={(e) => (
              <>
                <Cell className="justify-center">{e.success ? <CircleCheck className="size-3.5 text-ok" /> : <CircleX className="size-3.5 text-danger" />}</Cell>
                <Cell title={absoluteTime(e.executedAt)} className="text-muted tabular-nums">{absoluteTime(e.executedAt)}</Cell>
                <ConnCell id={e.connectionId} />
                <Cell className="text-muted">{e.database ?? ""}</Cell>
                <Cell className="justify-end text-muted tabular-nums">{formatDuration(e.elapsedMs)}</Cell>
                <Cell className={clsx("font-mono text-xs", !e.success && "text-danger/90")}>{firstLine(e.query)}</Cell>
              </>
            )}
          />
        ) : (
          <VirtualTable
            rows={savedRows}
            cols={SAVED_COLS}
            headers={["", "Name", "Connection", "Database", "Updated", "Query"]}
            selected={selected}
            onSelect={setSelected}
            onOpen={openSavedQuery}
            onContext={(ev, q) => menu.show(ev, savedMenu(q))}
            empty={loading ? null : (
              <Empty icon={<FileCode2 className="size-8" />} title={search || connFilter ? "No matching saved queries" : "No saved queries"}>
                Use “Save” in a query tab to keep frequently used SQL here.
              </Empty>
            )}
            renderRow={(q) => (
              <>
                <Cell className="justify-center"><FileCode2 className="size-3.5 text-accent" /></Cell>
                <Cell className="font-medium">{q.name}</Cell>
                {q.connectionId ? <ConnCell id={q.connectionId} /> : <Cell className="text-faint">—</Cell>}
                <Cell className="text-muted">{q.database ?? ""}</Cell>
                <Cell className="text-muted tabular-nums" title={absoluteTime(q.updatedAt)}>{relativeTime(q.updatedAt)}</Cell>
                <Cell className="font-mono text-xs">{firstLine(q.query)}</Cell>
              </>
            )}
          />
        )}
      </div>

      {(selHistory || selSaved) && (
        <PreviewPane
          query={(selHistory ?? selSaved)!.query}
          error={selHistory?.error}
          meta={selHistory
            ? `${connectionName(selHistory.connectionId)}${selHistory.database ? ` · ${selHistory.database}` : ""} · ${absoluteTime(selHistory.executedAt)} · ${formatDuration(selHistory.elapsedMs)}`
            : `${selSaved!.name} · ${connectionName(selSaved!.connectionId)}${selSaved!.database ? ` · ${selSaved!.database}` : ""}`}
          actions={
            selHistory ? (
              <Button size="sm" variant="primary" onClick={() => openHistoryEntry(selHistory)}><Play className="size-3" />Open in query</Button>
            ) : (
              <>
                <Button size="sm" onClick={async () => { if (await renameSavedQuery(selSaved!)) refresh(); }}><Pencil className="size-3" />Rename</Button>
                <Button size="sm" variant="danger" onClick={async () => { if (await deleteSavedQuery(selSaved!)) { setSelected(null); refresh(); } }}><Trash2 className="size-3" />Delete</Button>
                <Button size="sm" variant="primary" onClick={() => openSavedQuery(selSaved!)}><Play className="size-3" />Open</Button>
              </>
            )
          }
        />
      )}
      {menu.element}
    </div>
  );
}

function VirtualTable<T extends { id: string }>({ rows, cols, headers, selected, onSelect, onOpen, onContext, renderRow, empty }: {
  rows: T[];
  cols: string;
  headers: string[];
  selected: string | null;
  onSelect: (id: string) => void;
  onOpen: (row: T) => void;
  onContext: (e: React.MouseEvent, row: T) => void;
  renderRow: (row: T) => ReactNode;
  empty: ReactNode;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const v = useVirtualizer({ count: rows.length, getScrollElement: () => scrollRef.current, estimateSize: () => ROW_H, overscan: 12 });
  const selIndex = rows.findIndex((r) => r.id === selected);

  const onKeyDown = (e: KeyboardEvent) => {
    if (!rows.length) return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const next = Math.max(0, Math.min(rows.length - 1, selIndex < 0 ? 0 : selIndex + (e.key === "ArrowDown" ? 1 : -1)));
      onSelect(rows[next].id);
      v.scrollToIndex(next, { align: "auto" });
    } else if (e.key === "Enter" && selIndex >= 0) {
      e.preventDefault();
      onOpen(rows[selIndex]);
    }
  };

  return (
    <div className="flex h-full flex-col">
      <div className="grid h-7 shrink-0 items-center border-b border-line bg-app text-[11px] font-semibold tracking-wide text-muted uppercase" style={{ gridTemplateColumns: cols }}>
        {headers.map((h, i) => <div key={i} className="truncate px-2">{h}</div>)}
      </div>
      {rows.length === 0 ? (
        <div className="min-h-0 flex-1">{empty}</div>
      ) : (
        <div ref={scrollRef} tabIndex={0} onKeyDown={onKeyDown} className="min-h-0 flex-1 overflow-auto focus:outline-none">
          <div className="relative w-full" style={{ height: v.getTotalSize() }}>
            {v.getVirtualItems().map((vi) => {
              const row = rows[vi.index];
              return (
                <div
                  key={row.id}
                  onClick={() => onSelect(row.id)}
                  onDoubleClick={() => onOpen(row)}
                  onContextMenu={(e) => { onSelect(row.id); onContext(e, row); }}
                  className={clsx(
                    "absolute left-0 grid w-full items-center border-b border-line/40 text-[13px]",
                    row.id === selected ? "bg-accent/12 text-fg" : vi.index % 2 ? "bg-app/30 hover:bg-hover" : "hover:bg-hover",
                  )}
                  style={{ top: vi.start, height: ROW_H, gridTemplateColumns: cols }}
                >
                  {renderRow(row)}
                </div>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}

function Cell({ children, className, title }: { children?: ReactNode; className?: string; title?: string }) {
  return <div title={title} className={clsx("flex h-full min-w-0 items-center truncate px-2", className)}><span className="truncate">{children}</span></div>;
}

function ConnCell({ id }: { id: string }) {
  const conn = useConnections((s) => s.connections.find((c) => c.id === id));
  return (
    <div className="flex h-full min-w-0 items-center gap-1.5 px-2">
      {conn ? <KindIcon kind={conn.kind} className="size-3.5 shrink-0" /> : null}
      <span className={clsx("truncate", !conn && "text-faint italic")}>{conn?.name ?? "Deleted connection"}</span>
    </div>
  );
}

function PreviewPane({ query, error, meta, actions }: { query: string; error?: string | null; meta: string; actions: ReactNode }) {
  return (
    <div className="flex h-56 shrink-0 flex-col border-t border-line bg-app">
      <div className="flex h-8 shrink-0 items-center gap-2 border-b border-line px-3">
        <span className="min-w-0 flex-1 truncate text-xs text-muted">{meta}</span>
        <Button size="sm" variant="ghost" onClick={() => copyText(query)}><Copy className="size-3" />Copy</Button>
        {actions}
      </div>
      {error && (
        <div className="selectable max-h-20 shrink-0 overflow-auto border-b border-line bg-danger/10 px-3 py-1.5 font-mono text-xs whitespace-pre-wrap text-danger">{error}</div>
      )}
      <pre className="selectable min-h-0 flex-1 overflow-auto px-3 py-2 font-mono text-xs whitespace-pre-wrap text-fg/90">{query}</pre>
    </div>
  );
}
