// Paged, filterable, editable view of a table's rows.
import MiniSelect from "../grid/MiniSelect";
import { save } from "@tauri-apps/plugin-dialog";
import {
  ArrowLeftRight, Check, ChevronDown, ChevronFirst, ChevronLast, ChevronLeft, ChevronRight, CopyPlus, Download,
  Filter, FilterX, Lock, PanelRight, PencilRuler, Plus, RefreshCw, Trash2, Undo2,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Group, Panel, Separator } from "react-resizable-panels";
import { toast } from "sonner";
import { Badge, Button, ErrorBox, IconButton, Spinner, Spin } from "../../components/ui";
import { api, errMsg } from "../../lib/api";
import { dialectOf, type CellValue, type ColumnInfo, type RowChange, type SortSpec, type TableDataPage } from "../../lib/types";
import { useConnections } from "../../store/connections";
import { useTabs } from "../../store/tabs";
import type { Tab } from "../../tabs/types";
import { useContextMenu } from "../connections/ContextMenu";
import { confirmDialog } from "../connections/dialogs";
import { openDataTransfer, openTableDesign } from "../connections/openers";
import DataGrid from "../grid/DataGrid";
import { useGridController } from "../grid/useGridController";
import { ensureOpen } from "../query/useConnectionMeta";
import FilterBuilder from "./data/FilterBuilder";
import { containsSql, equalsSql } from "./data/filterSql";
import FormView from "./data/FormView";
import { qualifiedName } from "./sqlDialect";

type TableDataT = Extract<Tab, { type: "table-data" }>;

const NO_COLS: ColumnInfo[] = [];
const NO_ROWS: CellValue[][] = [];
const NO_PK: string[] = [];
const PAGE_SIZES = [100, 500, 1000, 5000];

export default function TableDataTab({ tab }: { tab: TableDataT }) {
  const conn = useConnections((s) => s.connections.find((c) => c.id === tab.connId));
  const dialect = conn ? dialectOf(conn.kind) : "none";
  const { table } = tab;

  const [page, setPage] = useState<TableDataPage | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [offset, setOffset] = useState(0);
  const [limit, setLimit] = useState(500);
  const [sort, setSort] = useState<SortSpec[]>([]);
  const [filter, setFilter] = useState("");
  const [filterDraft, setFilterDraft] = useState("");
  /** Per-column "contains" filters from the grid header, ANDed with the WHERE bar. */
  const [colFilters, setColFilters] = useState<Record<string, string>>({});
  const [builderOpen, setBuilderOpen] = useState(false);
  const [formView, setFormView] = useState(false);
  const [committing, setCommitting] = useState(false);
  const reqSeq = useRef(0);
  const exportMenu = useContextMenu();

  const columns = page?.result.columns ?? NO_COLS;
  const rows = page?.result.rows ?? NO_ROWS;
  const primaryKey = page?.primaryKey ?? NO_PK;
  const readOnlyConn = !!conn?.readOnly;

  const where = useMemo(() => {
    const parts = Object.entries(colFilters).map(([col, text]) => containsSql(dialect, col, text));
    const bar = filter.trim();
    if (bar) parts.unshift(parts.length ? `(${bar})` : bar);
    return parts.join(" AND ");
  }, [colFilters, filter, dialect]);
  const filtered = !!filter || Object.keys(colFilters).length > 0;

  const ctl = useGridController({ columns, rows, primaryKey, editable: !readOnlyConn, sort, onSortChange: (s) => guard(() => { setSort(s); setOffset(0); }) });

  // Mark tab dirty while edits are pending.
  const update = useTabs((s) => s.update);
  const dirty = ctl.pendingCount > 0;
  useEffect(() => { update(tab.id, { dirty }); }, [dirty, tab.id, update]);

  const load = useCallback(async () => {
    const seq = ++reqSeq.current;
    setLoading(true);
    setError(null);
    try {
      await ensureOpen(tab.connId);
      const res = await api.tableData(tab.connId, { table, offset, limit, filter: where || null, sort });
      if (seq === reqSeq.current) setPage(res);
    } catch (e) {
      if (seq === reqSeq.current) setError(errMsg(e));
    } finally {
      if (seq === reqSeq.current) setLoading(false);
    }
  }, [tab.connId, table, offset, limit, where, sort]);

  useEffect(() => { void load(); }, [load]);

  /** Run `fn` only after confirming that pending edits may be discarded. */
  const guardRef = useRef(ctl.pendingCount);
  guardRef.current = ctl.pendingCount;
  const guard = (fn: () => void) => {
    if (!guardRef.current) { fn(); return; }
    void confirmDialog({
      title: "Discard pending changes?",
      message: `${guardRef.current} uncommitted change${guardRef.current === 1 ? "" : "s"} will be lost.`,
      confirmLabel: "Discard",
      danger: true,
    }).then((ok) => ok && fn());
  };

  const commit = async (changes: RowChange[] = ctl.buildChanges()) => {
    if (!changes.length) return;
    setCommitting(true);
    try {
      const n = await api.applyChanges(tab.connId, table, changes);
      toast.success(`Committed ${changes.length} change${changes.length === 1 ? "" : "s"}`, { description: `${n} row${n === 1 ? "" : "s"} affected` });
      await load();
    } catch (e) {
      toast.error("Commit failed", { description: errMsg(e), duration: 8000 });
    } finally {
      setCommitting(false);
    }
  };

  const applyFilter = (f: string) => guard(() => { setFilterDraft(f); setFilter(f); setOffset(0); });
  const applyColumnFilter = (column: string, text: string) => {
    if ((colFilters[column] ?? "") === (text.trim() ? text : "")) return;
    guard(() => {
      setColFilters((cur) => {
        const { [column]: _old, ...rest } = cur;
        return text.trim() ? { ...rest, [column]: text } : rest;
      });
      setOffset(0);
    });
  };
  const clearFilters = () => guard(() => { setFilterDraft(""); setFilter(""); setColFilters({}); setOffset(0); });
  const addCondition = (cond: string) => {
    const cur = filter.trim();
    applyFilter(cur ? `(${cur}) AND ${cond}` : cond);
  };

  const quickExport = async (format: "csv" | "json" | "sql" | "xlsx") => {
    const path = await save({ defaultPath: `${table.name}.${format}`, filters: [{ name: format.toUpperCase(), extensions: [format] }] });
    if (!path) return;
    const t = toast.loading(`Exporting ${table.name}…`);
    try {
      const n = await api.exportData({
        connId: tab.connId,
        database: table.database ?? null,
        format,
        path,
        ...(where ? { query: `SELECT * FROM ${qualifiedName(dialect, table)} WHERE ${where}` } : { table }),
      });
      toast.success(`Exported ${n.toLocaleString()} rows`, { id: t, description: path });
    } catch (e) {
      toast.error("Export failed", { id: t, description: errMsg(e) });
    }
  };

  const total = page?.total ?? null;
  const lastOffset = total !== null ? Math.max(0, Math.floor((total - 1) / limit) * limit) : null;
  const hasNext = total !== null ? offset + limit < total : rows.length === limit;
  const selRows = ctl.selectedDataRows();
  const noPk = !!page && primaryKey.length === 0;

  if (!conn) return <ErrorBox error="Connection not found." />;

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "F5" || ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "r")) {
      e.preventDefault();
      guard(() => void load());
    }
  };

  return (
    <div className="flex h-full flex-col" onKeyDown={onKeyDown}>
      {conn.color && <div className="h-0.5 shrink-0" style={{ background: conn.color }} />}
      {/* toolbar */}
      <div className="flex h-9 shrink-0 items-center gap-1 border-b border-line bg-panel px-2">
        <IconButton title="Refresh (F5)" onClick={() => guard(() => void load())} disabled={loading}>
          <Spin icon={RefreshCw} spinning={loading} className="size-3.5" />
        </IconButton>
        <div className="mx-1 h-4 w-px bg-line" />
        <IconButton title="Add row" disabled={!ctl.canEdit} onClick={() => ctl.addRow()}><Plus className="size-4" /></IconButton>
        <IconButton title="Duplicate selected rows" disabled={!ctl.canEdit || !selRows.length} onClick={() => ctl.duplicateRows(selRows)}><CopyPlus className="size-3.5" /></IconButton>
        <IconButton title="Delete selected rows (⌘⌫)" disabled={!ctl.canEdit || !selRows.length} onClick={() => ctl.deleteRows(selRows)}><Trash2 className="size-3.5" /></IconButton>
        <Button size="sm" variant={dirty ? "primary" : "ghost"} disabled={!dirty} loading={committing} onClick={() => void commit()} title="Commit changes (⌘S)">
          <Check className="size-3.5" /> Commit{dirty ? ` (${ctl.pendingCount})` : ""}
        </Button>
        <IconButton title="Discard changes" disabled={!dirty} onClick={ctl.discard}><Undo2 className="size-3.5" /></IconButton>
        <div className="mx-1 h-4 w-px bg-line" />
        <IconButton title="Design table" onClick={() => openTableDesign(tab.connId, table)}><PencilRuler className="size-3.5" /></IconButton>
        <Button
          size="sm"
          variant="ghost"
          onClick={(e) => {
            const r = e.currentTarget.getBoundingClientRect();
            exportMenu.show({ clientX: r.left, clientY: r.bottom + 2 }, [
              { label: "Export CSV…", onClick: () => void quickExport("csv") },
              { label: "Export JSON…", onClick: () => void quickExport("json") },
              { label: "Export SQL INSERTs…", onClick: () => void quickExport("sql") },
              { label: "Export Excel…", onClick: () => void quickExport("xlsx") },
              "separator",
              { label: "Data transfer wizard…", icon: <ArrowLeftRight className="size-3.5" />, onClick: () => openDataTransfer(tab.connId, table.database, table.schema) },
            ]);
          }}
        >
          <Download className="size-3.5" /> Export <ChevronDown className="size-3" />
        </Button>
        <IconButton title="Form view" active={formView} onClick={() => setFormView((v) => !v)}><PanelRight className="size-3.5" /></IconButton>
        <div className="flex-1" />
        {readOnlyConn ? (
          <Badge tone="warn"><Lock className="mr-1 size-3" />read-only connection</Badge>
        ) : noPk ? (
          <span title="Rows can't be identified for updates without a primary key"><Badge tone="warn"><Lock className="mr-1 size-3" />no primary key · read-only</Badge></span>
        ) : null}
      </div>

      {/* filter bar */}
      <div className="relative flex h-8 shrink-0 items-center gap-1.5 border-b border-line bg-panel px-2">
        <span className="font-mono text-[11px] font-semibold text-accent">WHERE</span>
        <input
          spellCheck={false}
          className="h-6 min-w-0 flex-1 rounded border border-transparent bg-transparent px-1.5 font-mono text-xs text-fg placeholder:text-faint focus:border-line focus:bg-app focus:outline-none"
          placeholder="SQL expression, e.g. status = 'active' AND created_at > now() - interval '7 days'   (Enter to apply)"
          value={filterDraft}
          onChange={(e) => setFilterDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") applyFilter(filterDraft.trim());
            if (e.key === "Escape") setFilterDraft(filter);
          }}
        />
        {filterDraft !== filter && <span className="text-[10px] text-faint">↵ apply</span>}
        {Object.keys(colFilters).length > 0 && (
          <span className="text-[10px] text-accent" title={Object.entries(colFilters).map(([c, t]) => `${c} contains "${t}"`).join("\n")}>
            + {Object.keys(colFilters).length} column filter{Object.keys(colFilters).length === 1 ? "" : "s"}
          </span>
        )}
        {filtered && (
          <IconButton title="Clear all filters" onClick={clearFilters}><FilterX className="size-3.5" /></IconButton>
        )}
        <Button size="sm" variant="ghost" onClick={() => setBuilderOpen((v) => !v)} disabled={!columns.length}>
          <Filter className="size-3.5" /> Builder
        </Button>
        {builderOpen && (
          <FilterBuilder columns={columns} dialect={dialect} onClose={() => setBuilderOpen(false)} onApply={(w) => addCondition(w)} />
        )}
      </div>

      {/* grid */}
      <div className="relative min-h-0 flex-1">
        {error && !page ? (
          <div className="flex h-full flex-col">
            <ErrorBox error={error} />
            <div className="px-3"><Button size="sm" onClick={() => void load()}>Retry</Button></div>
          </div>
        ) : !page ? (
          <div className="flex h-full items-center justify-center"><Spinner /></div>
        ) : (
          <Group orientation="horizontal" className="h-full">
            <Panel minSize="30%">
              <DataGrid
                columns={columns}
                rows={rows}
                controller={ctl}
                rowNumberOffset={offset}
                dialect={dialect}
                tableName={qualifiedName(dialect, table)}
                onCommit={(c) => commit(c)}
                onFilterByValue={(col, v) => addCondition(equalsSql(dialect, col, v))}
                columnFilters={colFilters}
                onColumnFilter={applyColumnFilter}
                footer={
                  <Pager
                    offset={offset} limit={limit} total={total} count={rows.length} hasNext={hasNext} lastOffset={lastOffset}
                    onOffset={(o) => guard(() => setOffset(o))}
                    onLimit={(l) => guard(() => { setLimit(l); setOffset(0); })}
                  />
                }
              />
            </Panel>
            {formView && (
              <>
                <Separator className="w-px bg-line hover:bg-accent data-[separator=active]:bg-accent" />
                <Panel defaultSize="32%" minSize="18%" maxSize="60%">
                  <FormView ctl={ctl} />
                </Panel>
              </>
            )}
          </Group>
        )}
        {error && page && (
          <div className="absolute inset-x-0 bottom-6 z-30 m-2 flex items-start gap-2 rounded-md border border-danger/40 bg-panel p-2 font-mono text-xs text-danger shadow-xl">
            <span className="selectable flex-1 whitespace-pre-wrap">{error}</span>
            <button className="text-muted hover:text-fg" onClick={() => setError(null)}>dismiss</button>
          </div>
        )}
        {loading && page && <div className="pointer-events-none absolute inset-x-0 top-0 h-0.5 animate-pulse bg-accent" />}
      </div>
    </div>
  );
}

function Pager({ offset, limit, total, count, hasNext, lastOffset, onOffset, onLimit }: {
  offset: number; limit: number; total: number | null; count: number; hasNext: boolean; lastOffset: number | null;
  onOffset: (o: number) => void; onLimit: (l: number) => void;
}) {
  const pageNo = Math.floor(offset / limit) + 1;
  const pages = total !== null ? Math.max(1, Math.ceil(total / limit)) : null;
  return (
    <span className="flex items-center gap-1 whitespace-nowrap">
      <span className="mr-1 tabular-nums">
        {count ? `${(offset + 1).toLocaleString()}–${(offset + count).toLocaleString()}` : "0"}
        {total !== null && ` of ${total.toLocaleString()}`}
      </span>
      <IconButton title="First page" className="size-5" disabled={offset === 0} onClick={() => onOffset(0)}><ChevronFirst className="size-3.5" /></IconButton>
      <IconButton title="Previous page" className="size-5" disabled={offset === 0} onClick={() => onOffset(Math.max(0, offset - limit))}><ChevronLeft className="size-3.5" /></IconButton>
      <span className="tabular-nums">{pageNo}{pages !== null && ` / ${pages.toLocaleString()}`}</span>
      <IconButton title="Next page" className="size-5" disabled={!hasNext} onClick={() => onOffset(offset + limit)}><ChevronRight className="size-3.5" /></IconButton>
      <IconButton title="Last page" className="size-5" disabled={lastOffset === null || offset >= lastOffset} onClick={() => lastOffset !== null && onOffset(lastOffset)}><ChevronLast className="size-3.5" /></IconButton>
      <MiniSelect className="ml-1 h-5 text-[11px]" value={limit} onChange={(e) => onLimit(Number(e.target.value))} title="Rows per page">
        {PAGE_SIZES.map((n) => <option key={n} value={n}>{n} / page</option>)}
      </MiniSelect>
    </span>
  );
}
