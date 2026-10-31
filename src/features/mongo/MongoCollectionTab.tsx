import clsx from "clsx";
import {
  Braces, ChevronLeft, ChevronRight, ChevronsLeft, ChevronsRight, FileJson, Filter, Layers, ListTree, Play, Plus, RefreshCw, RotateCcw, Rows3, Table2,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import { Button, Dialog, Empty, ErrorBox, IconButton, Input, Select, Spinner, Spin } from "../../components/ui";
import { api, errMsg } from "../../lib/api";
import type { CellValue, QueryResult } from "../../lib/types";
import { useConnections } from "../../store/connections";
import type { Tab } from "../../tabs/types";
import { lsGet, lsSet } from "../transfer/shared";
import { AggregationView } from "./AggregationView";
import { DocEditorDialog } from "./DocEditorDialog";
import { collRef, compact, toJsonText, validateDoc } from "./ejson";
import { IndexesPanel } from "./IndexesPanel";
import { DocumentsView, JsonView, TableView, type Doc } from "./views";

type MongoTab = Extract<Tab, { type: "mongo-collection" }>;
type Section = "data" | "aggregation" | "indexes";
type View = "documents" | "table" | "json";

interface Query { filter: string; sort: string; projection: string }
const EMPTY_Q: Query = { filter: "", sort: "", projection: "" };

function parseJsonMessage(r?: QueryResult): unknown {
  try { return r?.message ? JSON.parse(r.message) : undefined; } catch { return undefined; }
}

export default function MongoCollectionTab({ tab }: { tab: MongoTab }) {
  const conn = useConnections((s) => s.connections.find((c) => c.id === tab.connId));
  const readOnly = !!conn?.readOnly;
  const table = tab.table;
  const prefKey = `mongo:coll:${tab.connId}:${table.database ?? ""}:${table.name}`;
  const prefs = lsGet<{ view?: View; limit?: number; query?: Query; expand?: number }>(prefKey, {});

  const [section, setSection] = useState<Section>("data");
  const [view, setView] = useState<View>(prefs.view ?? "documents");
  const [draft, setDraft] = useState<Query>(prefs.query ?? EMPTY_Q);
  const [query, setQuery] = useState<Query>(prefs.query ?? EMPTY_Q);
  const [limit, setLimit] = useState(prefs.limit ?? 50);
  const [offset, setOffset] = useState(0);
  const [expand, setExpand] = useState(prefs.expand ?? 1);
  const [showProjection, setShowProjection] = useState(!!prefs.query?.projection);
  const [docs, setDocs] = useState<Doc[]>([]);
  const [total, setTotal] = useState<number | null>(null);
  const [ms, setMs] = useState(0);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [editor, setEditor] = useState<{ mode: "edit" | "insert"; doc?: Doc; text: string } | null>(null);
  const [toDelete, setToDelete] = useState<Doc | null>(null);
  const [nonce, setNonce] = useState(0);

  useEffect(() => { lsSet(prefKey, { view, limit, query, expand }); }, [prefKey, view, limit, query, expand]);

  const errors = useMemo(() => ({
    filter: validateDoc(draft.filter),
    sort: validateDoc(draft.sort),
    projection: validateDoc(draft.projection),
  }), [draft]);
  const invalid = !!(errors.filter || errors.sort || errors.projection);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    const f = query.filter.trim() || "{}";
    const stages = [`{ "$match": ${f} }`];
    if (query.sort.trim()) stages.push(`{ "$sort": ${query.sort.trim()} }`);
    stages.push(`{ "$skip": ${offset} }`, `{ "$limit": ${limit} }`);
    if (query.projection.trim()) stages.push(`{ "$project": ${query.projection.trim()} }`);
    stages.push('{ "$project": { "_id": 0, "__doc": "$$ROOT" } }');
    const count = query.filter.trim() && query.filter.trim() !== "{}"
      ? `${collRef(table.name)}.countDocuments(${f})`
      : `${collRef(table.name)}.estimatedDocumentCount()`;
    const script = `${collRef(table.name)}.aggregate([${stages.join(", ")}]);\n${count}`;
    const t0 = performance.now();
    try {
      const [r, c] = await api.execute(tab.connId, script, { database: table.database, maxRows: limit, skipHistory: true });
      const idx = r?.columns.findIndex((col) => col.name === "__doc") ?? -1;
      setDocs(idx >= 0 ? r.rows.map((row) => row[idx] as Doc) : []);
      const n = parseJsonMessage(c);
      setTotal(typeof n === "number" ? n : null);
      setMs(r?.elapsedMs ?? Math.round(performance.now() - t0));
    } catch (e) {
      setError(errMsg(e));
      setDocs([]);
    } finally {
      setLoading(false);
    }
  }, [tab.connId, table, query, offset, limit]);
  useEffect(() => { if (section === "data") load(); }, [load, section, nonce]);

  const apply = () => {
    if (invalid) return;
    setOffset(0);
    setQuery({ ...draft });
    setNonce((n) => n + 1);
  };
  const reset = () => { setDraft(EMPTY_Q); setQuery(EMPTY_Q); setOffset(0); };

  // ---- writes
  const exec = async (stmt: string) => {
    const [r] = await api.execute(tab.connId, stmt, { database: table.database });
    return parseJsonMessage(r) as Record<string, unknown> | undefined;
  };
  const saveDoc = async (doc: Doc) => {
    if (editor?.mode === "edit") {
      const res = await exec(`${collRef(table.name)}.replaceOne(${JSON.stringify({ _id: editor.doc!._id })}, ${toJsonText(doc, false)})`);
      if (res && res.matchedCount === 0) throw new Error("Document not found — it may have been deleted.");
      toast.success("Document updated");
    } else {
      const res = await exec(`${collRef(table.name)}.insertOne(${toJsonText(doc, false)})`);
      toast.success(`Inserted ${res?.insertedId !== undefined ? compact(res.insertedId) : "document"}`);
    }
    setEditor(null);
    load();
  };
  const doDelete = async () => {
    const d = toDelete;
    setToDelete(null);
    if (!d) return;
    try {
      await api.applyChanges(tab.connId, table, [{ op: "delete", key: { _id: d._id as CellValue } }]);
      toast.success("Document deleted");
      load();
    } catch (e) {
      toast.error(`Delete failed: ${errMsg(e)}`);
    }
  };

  const actions = readOnly ? {} : {
    onEdit: (d: Doc) => setEditor({ mode: "edit", doc: d, text: toJsonText(d) }),
    onDuplicate: (d: Doc) => {
      const { _id: _ignored, ...rest } = d;
      void _ignored;
      setEditor({ mode: "insert", text: toJsonText(rest) });
    },
    onDelete: (d: Doc) => setToDelete(d),
  };

  const pages = total != null ? Math.max(1, Math.ceil(total / limit)) : null;
  const page = Math.floor(offset / limit) + 1;
  const hasNext = total != null ? offset + limit < total : docs.length === limit;

  const sectionBtn = (s: Section, label: string, Icon: typeof Rows3) => (
    <button onClick={() => setSection(s)}
      className={clsx("flex h-9 items-center gap-1.5 border-b-2 px-3 text-xs", section === s ? "border-accent text-fg" : "border-transparent text-muted hover:text-fg")}>
      <Icon className="size-3.5" />{label}
    </button>
  );

  return (
    <div className="flex h-full flex-col">
      <div className="flex h-9 shrink-0 items-center border-b border-line bg-panel pr-2">
        {sectionBtn("data", "Documents", Rows3)}
        {sectionBtn("aggregation", "Aggregation", Layers)}
        {sectionBtn("indexes", "Indexes & fields", ListTree)}
        <div className="flex-1" />
        <span className="truncate font-mono text-[11px] text-faint">{table.database}.{table.name}</span>
      </div>

      {section === "data" && (
        <>
          <div className="flex min-h-9 shrink-0 flex-wrap items-center gap-1 gap-y-1 border-b border-line bg-panel px-2 py-1">
            <div className="flex rounded-md border border-line p-0.5">
              {([["documents", Braces, "Documents"], ["table", Table2, "Table"], ["json", FileJson, "JSON"]] as const).map(([v, Icon, l]) => (
                <button key={v} onClick={() => setView(v)} title={l}
                  className={clsx("flex h-5 items-center gap-1 rounded px-2 text-xs", view === v ? "bg-hover text-fg" : "text-muted hover:text-fg")}>
                  <Icon className="size-3.5" />{l}
                </button>
              ))}
            </div>
            <QueryInput label="Filter" icon={<Filter className="size-3" />} value={draft.filter} error={errors.filter} placeholder='{ "status": "active" }'
              onChange={(filter) => setDraft({ ...draft, filter })} onEnter={apply} className="min-w-64 flex-[3]" />
            <QueryInput label="Sort" value={draft.sort} error={errors.sort} placeholder='{ "_id": -1 }'
              onChange={(sort) => setDraft({ ...draft, sort })} onEnter={apply} className="min-w-36 flex-1" />
            {showProjection ? (
              <QueryInput label="Project" value={draft.projection} error={errors.projection} placeholder='{ "name": 1 }'
                onChange={(projection) => setDraft({ ...draft, projection })} onEnter={apply} className="min-w-36 flex-1" />
            ) : (
              <Button size="sm" variant="ghost" onClick={() => setShowProjection(true)}>+ Projection</Button>
            )}
            <Button size="sm" variant="primary" onClick={apply} disabled={invalid} title="Run (Enter)"><Play className="size-3.5" />Find</Button>
            <IconButton title="Reset query" onClick={reset}><RotateCcw className="size-3.5" /></IconButton>
            <div className="mx-1 h-4 w-px bg-line" />
            <Button size="sm" onClick={() => setEditor({ mode: "insert", text: "{\n  \n}" })} disabled={readOnly}><Plus className="size-3.5" />Insert</Button>
            <IconButton title="Reload" onClick={load}><Spin icon={RefreshCw} spinning={loading} className="size-3.5" /></IconButton>
          </div>

          <div className="relative min-h-0 flex-1 bg-app">
            {error && <ErrorBox error={error} />}
            {!error && loading && !docs.length && <div className="flex h-full items-center justify-center gap-2 text-muted"><Spinner />Loading documents…</div>}
            {!error && !loading && !docs.length && (
              <Empty icon={<Braces className="size-8" />} title={query.filter ? "No documents match the filter" : "Collection is empty"}>
                {!readOnly && <Button size="sm" className="mt-2" onClick={() => setEditor({ mode: "insert", text: "{\n  \n}" })}><Plus className="size-3.5" />Insert document</Button>}
              </Empty>
            )}
            {!error && docs.length > 0 && (
              view === "documents" ? <DocumentsView docs={docs} offset={offset} expand={expand} actions={actions} /> :
                view === "table" ? <TableView docs={docs} offset={offset} actions={actions} /> : <JsonView docs={docs} />
            )}
            {loading && docs.length > 0 && <div className="absolute right-3 top-2"><Spinner /></div>}
          </div>

          <div className="flex h-8 shrink-0 items-center gap-1 border-t border-line bg-panel px-2 text-xs text-muted">
            <IconButton title="First page" disabled={offset === 0 || loading} onClick={() => setOffset(0)}><ChevronsLeft className="size-3.5" /></IconButton>
            <IconButton title="Previous page" disabled={offset === 0 || loading} onClick={() => setOffset(Math.max(0, offset - limit))}><ChevronLeft className="size-3.5" /></IconButton>
            <span className="px-1">
              {docs.length ? `${(offset + 1).toLocaleString()}–${(offset + docs.length).toLocaleString()}` : "0"}
              {total != null && ` of ${total.toLocaleString()}`}
              {pages != null && <span className="text-faint"> · page {page}/{pages}</span>}
            </span>
            <IconButton title="Next page" disabled={!hasNext || loading} onClick={() => setOffset(offset + limit)}><ChevronRight className="size-3.5" /></IconButton>
            <IconButton title="Last page" disabled={!hasNext || loading || total == null} onClick={() => total != null && setOffset(Math.max(0, (pages! - 1) * limit))}><ChevronsRight className="size-3.5" /></IconButton>
            <Select value={String(limit)} onChange={(e) => { setLimit(Number(e.target.value)); setOffset(0); }} className="ml-1 h-6 w-20 text-xs" title="Page size">
              {[20, 50, 100, 200, 500].map((n) => <option key={n} value={n}>{n} / page</option>)}
            </Select>
            {view === "documents" && (
              <Select value={String(expand)} onChange={(e) => setExpand(Number(e.target.value))} className="h-6 w-28 text-xs" title="Default expansion of nested fields">
                <option value="0">Collapsed</option>
                <option value="1">Expand 1 level</option>
                <option value="2">Expand 2 levels</option>
                <option value="99">Expand all</option>
              </Select>
            )}
            <div className="flex-1" />
            {!loading && !error && <span className="text-faint">{ms} ms</span>}
            {readOnly && <span className="text-warn">read-only</span>}
          </div>
        </>
      )}
      {section === "aggregation" && <div className="min-h-0 flex-1"><AggregationView connId={tab.connId} table={table} readOnly={readOnly} /></div>}
      {section === "indexes" && <div className="min-h-0 flex-1"><IndexesPanel connId={tab.connId} table={table} readOnly={readOnly} /></div>}

      {editor && (
        <DocEditorDialog
          title={editor.mode === "edit" ? `Edit document ${compact(editor.doc?._id, 60)}` : `Insert into ${table.name}`}
          initial={editor.text}
          idLocked={editor.mode === "edit" ? JSON.stringify(editor.doc?._id) : undefined}
          onClose={() => setEditor(null)}
          onSave={saveDoc}
        />
      )}
      <Dialog open={!!toDelete} onClose={() => setToDelete(null)} title="Delete document?" width={460}
        footer={<><Button onClick={() => setToDelete(null)}>Cancel</Button><Button variant="danger" onClick={doDelete} autoFocus>Delete</Button></>}>
        <p className="text-[13px]">Delete the document with <span className="font-mono text-accent">_id: {compact(toDelete?._id, 100)}</span> from <span className="font-mono">{table.name}</span>? This cannot be undone.</p>
      </Dialog>
    </div>
  );
}

function QueryInput({ label, icon, value, onChange, onEnter, error, placeholder, className }: {
  label: string; icon?: React.ReactNode; value: string; onChange: (v: string) => void; onEnter: () => void; error: string | null; placeholder?: string; className?: string;
}) {
  return (
    <label className={clsx("flex items-center gap-1.5", className)} title={error ?? undefined}>
      <span className="flex shrink-0 items-center gap-1 text-[11px] text-muted">{icon}{label}</span>
      <Input
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => { if (e.key === "Enter") onEnter(); }}
        placeholder={placeholder}
        className={clsx("h-6 font-mono text-xs", error && "border-danger focus:border-danger")}
      />
    </label>
  );
}
