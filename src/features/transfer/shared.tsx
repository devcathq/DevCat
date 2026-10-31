// Shared helpers for tool pages (data transfer, schema compare, ER):
// connection/database/schema pickers, table discovery, progress events, persisted prefs.
import { listen } from "@tauri-apps/api/event";
import clsx from "clsx";
import { CheckSquare, RefreshCw, Search, Square } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { IconButton, Input, Select, Spinner, Spin } from "../../components/ui";
import { KindIcon } from "../../components/ui/KindIcon";
import { api, errMsg } from "../../lib/api";
import type { ConnectionConfig, ConnectionKind, TableRef, TreeNode } from "../../lib/types";
import { useConnections, useScopedConnections } from "../../store/connections";

export const SQL_KINDS: ConnectionKind[] = ["postgres", "mysql", "sqlite", "mssql"];
export const SCHEMA_KINDS: ConnectionKind[] = ["postgres", "mssql"];

// ---------------------------------------------------------------------------
// localStorage prefs

export function lsGet<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(`devcat:${key}`);
    return raw === null ? fallback : (JSON.parse(raw) as T);
  } catch {
    return fallback;
  }
}

export function lsSet(key: string, value: unknown) {
  try { localStorage.setItem(`devcat:${key}`, JSON.stringify(value)); } catch { /* quota */ }
}

export function newJobId(prefix = "job") {
  return `${prefix}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
}

export const basename = (p: string) => p.split(/[\\/]/).pop() ?? p;
export const dirname = (p: string) => p.replace(/[\\/][^\\/]*$/, "");
export const joinPath = (dir: string, file: string) => `${dir.replace(/[\\/]+$/, "")}${dir.includes("\\") && !dir.includes("/") ? "\\" : "/"}${file}`;
export const safeFileName = (s: string) => s.replace(/[^\w.-]+/g, "_");

// ---------------------------------------------------------------------------
// Tree discovery

/** Path of a database node (falls back to `[db]`). */
async function dbPath(connId: string, db: string | null | undefined): Promise<string[]> {
  if (!db) return [];
  try {
    const roots = await api.children(connId, []);
    const n = roots.find((r) => r.kind === "database" && r.label === db);
    if (n) return n.path;
  } catch { /* fall through */ }
  return [db];
}

export async function listSchemas(connId: string, db: string | null | undefined): Promise<string[]> {
  const base = await dbPath(connId, db);
  const nodes = await api.children(connId, base);
  return nodes.filter((n) => n.kind === "schema").map((n) => n.label);
}

// Folder ids are the last path segment (stable, lowercase); labels are a fallback.
const folderId = (n: TreeNode) => n.path[n.path.length - 1] ?? n.label;
const isTableFolder = (n: TreeNode) => n.kind === "folder" && (folderId(n) === "tables" || /^tables?$/i.test(n.label));
const isCollectionFolder = (n: TreeNode) => n.kind === "folder" && (folderId(n) === "collections" || /^collections?$/i.test(n.label));

/**
 * Discover tables of a database/schema by walking the object tree:
 * database -> (schema) -> "Tables" folder -> table nodes. Defensive about layout.
 */
export async function listTables(connId: string, db: string | null | undefined, schema: string | null | undefined): Promise<TableRef[]> {
  let base = await dbPath(connId, db);
  let nodes = await api.children(connId, base);
  if (schema) {
    const sn = nodes.find((n) => n.kind === "schema" && n.label === schema);
    if (sn) {
      base = sn.path;
      nodes = await api.children(connId, base);
    }
  }
  const out: TableRef[] = [];
  const take = (ns: TreeNode[]) => {
    for (const n of ns) if (n.kind === "table" || n.kind === "collection") out.push(n.table ?? { database: db ?? null, schema: schema ?? null, name: n.label });
  };
  take(nodes);
  for (const f of nodes.filter((n) => isTableFolder(n) || isCollectionFolder(n))) take(await api.children(connId, f.path));
  // Unknown layout: one more level for any folder if nothing found yet.
  if (!out.length) {
    for (const f of nodes.filter((n) => n.kind === "folder" && n.hasChildren)) {
      const kids = await api.children(connId, f.path);
      take(kids);
    }
  }
  const seen = new Set<string>();
  return out.filter((t) => {
    const k = `${t.schema ?? ""}.${t.name}`;
    if (seen.has(k)) return false;
    seen.add(k);
    return true;
  });
}

// ---------------------------------------------------------------------------
// Hooks

export function useAsync<T>(fn: (() => Promise<T>) | null, deps: unknown[]) {
  const [state, setState] = useState<{ data?: T; loading: boolean; error?: string }>({ loading: false });
  const [nonce, setNonce] = useState(0);
  useEffect(() => {
    if (!fn) { setState({ loading: false }); return; }
    let alive = true;
    setState((s) => ({ data: s.data, loading: true }));
    fn().then(
      (data) => alive && setState({ data, loading: false }),
      (e) => alive && setState({ loading: false, error: errMsg(e) }),
    );
    return () => { alive = false; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, nonce]);
  const reload = useCallback(() => setNonce((n) => n + 1), []);
  return { ...state, reload };
}

export interface TransferProgress {
  jobId: string;
  rows: number;
  done: boolean;
  table?: string | null;
  /** total rows/statements when known */
  total?: number | null;
  /** multi-table jobs (copy): 0-based index of the current table and table count */
  tableIndex?: number;
  tables?: number;
  tableRows?: number;
  tableTotal?: number | null;
  failed?: number;
  error?: string;
}

/** Subscribe to `transfer-progress` events for one job id. */
export function useTransferProgress(jobId: string | null, onEvent?: (p: TransferProgress) => void) {
  const [progress, setProgress] = useState<TransferProgress | null>(null);
  const cb = useRef(onEvent);
  cb.current = onEvent;
  useEffect(() => {
    setProgress(null);
    if (!jobId) return;
    let un: (() => void) | undefined;
    let cancelled = false;
    listen<TransferProgress>("transfer-progress", (e) => {
      if (e.payload?.jobId !== jobId) return;
      setProgress(e.payload);
      cb.current?.(e.payload);
    }).then((u) => { if (cancelled) u(); else un = u; }).catch(() => { /* not in tauri */ });
    return () => { cancelled = true; un?.(); };
  }, [jobId]);
  return progress;
}

// ---------------------------------------------------------------------------
// Pickers

export interface Target { connId: string; database: string | null; schema: string | null }

export function ConnSelect({ value, onChange, kinds = SQL_KINDS, className, placeholder = "Select connection…" }:
  { value: string; onChange: (id: string) => void; kinds?: ConnectionKind[]; className?: string; placeholder?: string }) {
  const connections = useScopedConnections();
  const projects = useConnections((s) => s.projects);
  const list = connections.filter((c) => kinds.includes(c.kind));
  // Group options by project (connections without a project first).
  const groups = useMemo(() => {
    const m = new Map<string, ConnectionConfig[]>();
    for (const c of list) {
      const g = projects.find((p) => p.id === c.projectId)?.name ?? "";
      m.set(g, [...(m.get(g) ?? []), c]);
    }
    const order = ["", ...projects.map((p) => p.name)];
    return [...m.entries()].sort(([a], [b]) => order.indexOf(a) - order.indexOf(b));
  }, [list, projects]);
  return (
    <Select value={value} onChange={(e) => onChange(e.target.value)} className={className}>
      <option value="">{list.length ? placeholder : "No compatible connections"}</option>
      {groups.map(([g, cs]) => g
        ? <optgroup key={g} label={g}>{cs.map((c) => <option key={c.id} value={c.id}>{c.name}</option>)}</optgroup>
        : cs.map((c) => <option key={c.id} value={c.id}>{c.name}</option>))}
    </Select>
  );
}

/** Connection → database → schema cascade. */
export function TargetPicker({ value, onChange, kinds = SQL_KINDS, label, compact, allowNoSchema = true }:
  { value: Target; onChange: (t: Target) => void; kinds?: ConnectionKind[]; label?: string; compact?: boolean; allowNoSchema?: boolean }) {
  const conn = useConnections((s) => s.connections.find((c) => c.id === value.connId));
  const dbs = useAsync(value.connId ? () => api.listDatabases(value.connId) : null, [value.connId]);
  const hasSchemas = !!conn && SCHEMA_KINDS.includes(conn.kind);
  const schemas = useAsync(value.connId && hasSchemas && (value.database || conn?.kind === "mssql")
    ? () => listSchemas(value.connId, value.database) : null, [value.connId, value.database, hasSchemas]);

  // Auto-pick defaults
  useEffect(() => {
    if (!dbs.data || !value.connId) return;
    if (!value.database || !dbs.data.includes(value.database)) {
      const pref = conn?.database && dbs.data.includes(conn.database) ? conn.database : dbs.data[0] ?? null;
      if (pref !== value.database) onChange({ ...value, database: pref, schema: null });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dbs.data]);
  useEffect(() => {
    if (!schemas.data || value.schema && schemas.data.includes(value.schema)) return;
    const pref = ["public", "dbo"].find((s) => schemas.data!.includes(s)) ?? (allowNoSchema ? null : schemas.data[0] ?? null);
    if (pref !== value.schema) onChange({ ...value, schema: pref });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [schemas.data]);

  return (
    <div className={clsx("flex gap-2", compact ? "items-center" : "flex-col")}>
      {label && <div className="text-[11px] font-semibold uppercase tracking-wider text-muted">{label}</div>}
      <div className="flex min-w-0 flex-1 items-center gap-1.5">
        {conn ? <KindIcon kind={conn.kind} className="size-4 shrink-0" /> : null}
        <ConnSelect kinds={kinds} value={value.connId} onChange={(id) => onChange({ connId: id, database: null, schema: null })} className="min-w-0 flex-1" />
      </div>
      <div className="flex min-w-0 flex-1 items-center gap-1.5">
        <Select
          value={value.database ?? ""}
          disabled={!value.connId || dbs.loading}
          onChange={(e) => onChange({ ...value, database: e.target.value || null, schema: null })}
          className="min-w-0 flex-1"
          title={dbs.error}
        >
          {!value.connId && <option value="">Database</option>}
          {dbs.loading && <option value={value.database ?? ""}>Loading…</option>}
          {dbs.error && <option value="">(error) {dbs.error.slice(0, 60)}</option>}
          {dbs.data?.map((d) => <option key={d} value={d}>{d}</option>)}
        </Select>
        {hasSchemas && (
          <Select
            value={value.schema ?? ""}
            disabled={schemas.loading}
            onChange={(e) => onChange({ ...value, schema: e.target.value || null })}
            className="min-w-0 flex-1"
            title={schemas.error}
          >
            {allowNoSchema && <option value="">(default schema)</option>}
            {schemas.data?.map((d) => <option key={d} value={d}>{d}</option>)}
          </Select>
        )}
        <IconButton title="Reload databases" onClick={() => { dbs.reload(); schemas.reload(); }} disabled={!value.connId}>
          <Spin icon={RefreshCw} spinning={dbs.loading || schemas.loading} className="size-3.5" />
        </IconButton>
      </div>
    </div>
  );
}

/** Checkbox list of tables with filter / select all. */
export function TableChecklist({ tables, loading, error, selected, onChange, className, emptyText = "No tables", keyOf = (t) => t.name, labelOf = (t) => t.name }:
  {
    tables: TableRef[]; loading?: boolean; error?: string; selected: Set<string>; onChange: (s: Set<string>) => void; className?: string; emptyText?: string;
    keyOf?: (t: TableRef) => string; labelOf?: (t: TableRef) => string;
  }) {
  const [filter, setFilter] = useState("");
  const shown = useMemo(() => tables.filter((t) => labelOf(t).toLowerCase().includes(filter.toLowerCase())), [tables, filter, labelOf]);
  const allOn = shown.length > 0 && shown.every((t) => selected.has(keyOf(t)));
  const toggleAll = () => {
    const s = new Set(selected);
    shown.forEach((t) => (allOn ? s.delete(keyOf(t)) : s.add(keyOf(t))));
    onChange(s);
  };
  return (
    <div className={clsx("flex min-h-0 flex-col rounded-md border border-line bg-app", className)}>
      <div className="flex items-center gap-1 border-b border-line p-1">
        <IconButton title={allOn ? "Deselect all" : "Select all"} onClick={toggleAll}>
          {allOn ? <CheckSquare className="size-3.5" /> : <Square className="size-3.5" />}
        </IconButton>
        <div className="relative flex-1">
          <Search className="pointer-events-none absolute left-2 top-1.5 size-3.5 text-faint" />
          <Input value={filter} onChange={(e) => setFilter(e.target.value)} placeholder="Filter tables" className="h-6 border-none bg-transparent pl-7 text-xs" />
        </div>
        <span className="px-1 text-[11px] text-faint">{selected.size}/{tables.length}</span>
      </div>
      <div className="min-h-0 flex-1 overflow-auto py-0.5">
        {loading && <div className="flex items-center gap-2 p-3 text-xs text-muted"><Spinner className="size-3.5" />Loading tables…</div>}
        {error && <div className="p-3 text-xs text-danger">{error}</div>}
        {!loading && !error && !shown.length && <div className="p-3 text-xs text-faint">{emptyText}</div>}
        {shown.map((t) => (
          <label key={keyOf(t)} className="flex cursor-pointer items-center gap-2 px-2 py-0.5 text-[13px] hover:bg-hover">
            <input
              type="checkbox"
              className="accent-[var(--color-accent)]"
              checked={selected.has(keyOf(t))}
              onChange={(e) => {
                const s = new Set(selected);
                if (e.target.checked) s.add(keyOf(t)); else s.delete(keyOf(t));
                onChange(s);
              }}
            />
            <span className="truncate">{labelOf(t)}</span>
          </label>
        ))}
      </div>
    </div>
  );
}
