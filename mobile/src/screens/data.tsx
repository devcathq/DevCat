// Data: the computer's connections, a tree browser (databases → schemas → tables, Redis key
// groups, Mongo collections), rows as cards with edit, a query editor and the Redis key editor.
import clsx from "clsx";
import { Database, FileText, Folder, History as HistoryIcon, KeyRound, Lock, Pencil, Play, Plus, RefreshCw, Save, Search, Table2, Trash2 } from "lucide-react";
import { useEffect, useMemo, useState, type ReactNode } from "react";
import { host, nav, toast, toastErr, useApp } from "../lib/store";
import type {
  CellValue, ConnectionConfig, ConnectionKind, HistoryEntry, Project, QueryResult, RowChange, SavedQuery, TableDataPage, TableRef, TableStructure, TreeNode,
} from "../lib/types";
import { Btn, Card, Confirm, Dot, Empty, ErrorBox, Field, IconBtn, Input, Item, Loading, Note, Screen, Section, Seg, Sheet, TextArea, ago, useLoad } from "../ui";

// SSH sessions never reach the phone (the computer leaves them out: see CLAUDE.md); named for the type only.
const KIND_LABEL: Record<ConnectionKind, string> = {
  postgres: "PostgreSQL", mysql: "MySQL", sqlite: "SQLite", mssql: "SQL Server", mongodb: "MongoDB", redis: "Redis", kubernetes: "Kubernetes", rabbitmq: "RabbitMQ",
  ssh: "SSH session",
};
const KIND_ICON: Record<string, string> = { postgres: "🐘", mysql: "🐬", sqlite: "🪶", mssql: "🗄", mongodb: "🍃", redis: "🟥", kubernetes: "☸", rabbitmq: "🐇" };
const SQL_KINDS: ConnectionKind[] = ["postgres", "mysql", "sqlite", "mssql"];

function useConnections() {
  return useLoad(async () => {
    const [conns, projects] = await Promise.all([host<ConnectionConfig[]>("list_connections"), host<Project[]>("list_projects")]);
    return { conns, projects };
  }, [useApp.getState().current]);
}

export function useConn(connId: string): ConnectionConfig | undefined {
  const c = useLoad(() => host<ConnectionConfig[]>("list_connections"), [connId]);
  return c.data?.find((x) => x.id === connId);
}

const cellText = (v: CellValue): string => (v === null || v === undefined ? "NULL" : typeof v === "object" ? JSON.stringify(v) : String(v));

// ------------------------------------------------------------------ connections

export function Data() {
  const { data, error, loading, reload } = useConnections();
  const open = useLoad(() => host<string[]>("open_connection_ids"), [], 15000);
  const history = useLoad(() => host<HistoryEntry[]>("history_list", { connectionId: null, search: null, limit: 5 }), []);
  const [q, setQ] = useState("");
  const groups = useMemo(() => {
    if (!data) return [];
    const list = data.conns.filter((c) => c.kind !== "kubernetes" && c.kind !== "rabbitmq" && (!q || c.name.toLowerCase().includes(q.toLowerCase())));
    const byProject = new Map<string, ConnectionConfig[]>();
    for (const c of list) {
      const k = c.projectId ?? "";
      byProject.set(k, [...(byProject.get(k) ?? []), c]);
    }
    const ordered = [...data.projects].sort((a, b) => a.order - b.order).map((p) => ({ name: p.name, conns: byProject.get(p.id) ?? [] }));
    ordered.push({ name: "Other", conns: byProject.get("") ?? [] });
    return ordered.filter((g) => g.conns.length);
  }, [data, q]);
  const names = Object.fromEntries((data?.conns ?? []).map((c) => [c.id, c.name]));

  return (
    <Screen title="Data" sub={useApp.getState().machines.find((m) => m.id === useApp.getState().current)?.name} tab="data" right={<IconBtn label="Refresh" onClick={reload}><RefreshCw className="size-4" /></IconBtn>}>
      <Input placeholder="Search connections" value={q} onChange={(e) => setQ(e.target.value)} />
      {loading ? <Loading /> : error ? <ErrorBox error={error} onRetry={reload} /> : groups.length === 0 ? (
        <Empty icon={<Database className="size-8" />} title="No database connections">Add connections in DevCat on the computer; they show up here.</Empty>
      ) : groups.map((g) => (
        <div key={g.name} className="space-y-2">
          <Section>{g.name}</Section>
          {g.conns.map((c) => (
            <Item key={c.id} onClick={() => nav.push({ s: "browse", connId: c.id, path: [], title: c.name })}
              icon={<span className="text-[18px]">{KIND_ICON[c.kind]}</span>}
              title={c.name}
              meta={`${KIND_LABEL[c.kind]} · ${c.kind === "sqlite" ? c.filePath ?? "" : c.host || c.url || ""}${c.ssh?.enabled ? " · via SSH" : ""}`}
              right={<span className="flex items-center gap-1.5">{c.readOnly && <Lock className="size-3.5 text-faint" />}{open.data?.includes(c.id) && <Dot tone="ok" />}</span>}
              chevron />
          ))}
        </div>
      ))}
      {(history.data?.length ?? 0) > 0 && <Section>Recent queries</Section>}
      {history.data?.map((h) => (
        <Item key={h.id} onClick={() => nav.push({ s: "query", connId: h.connectionId, database: h.database, sql: h.query })}
          icon={<HistoryIcon className="size-4" />} title={<span className="font-mono text-[12px]">{h.query.replace(/\s+/g, " ").slice(0, 80)}</span>}
          meta={`${names[h.connectionId] ?? "connection"} · ${ago(h.executedAt)}`} />
      ))}
    </Screen>
  );
}

// ------------------------------------------------------------------ tree

function nodeIcon(n: TreeNode): ReactNode {
  switch (n.kind) {
    case "database": return <Database className="size-4" />;
    case "table": case "view": case "materializedView": case "collection": return <Table2 className="size-4" />;
    case "key": return <KeyRound className="size-4" />;
    case "keyGroup": case "folder": case "schema": return <Folder className="size-4" />;
    default: return <FileText className="size-4" />;
  }
}

export function Browse({ connId, path, title }: { connId: string; path: string[]; title: string }) {
  const conn = useConn(connId);
  const nodes = useLoad(() => host<TreeNode[]>("db_children", { connId, path }), [connId, path.join("\u0000")]);
  const [q, setQ] = useState("");
  const [prefix, setPrefix] = useState("");
  const isRedis = conn?.kind === "redis";
  const list = (nodes.data ?? []).filter((n) => !q || n.label.toLowerCase().includes(q.toLowerCase()));
  const open = (n: TreeNode) => {
    if (n.kind === "key") {
      nav.push({ s: "rediskey", connId, database: n.table?.database ?? n.path[0] ?? null, key: n.table?.name ?? n.path.slice(1).join("") });
    } else if (n.table && ["table", "view", "materializedView", "collection"].includes(n.kind)) {
      nav.push({ s: "rows", connId, table: n.table });
    } else if (n.hasChildren) {
      nav.push({ s: "browse", connId, path: n.path, title: n.kind === "keyGroup" ? `${n.label}:` : n.label });
    }
  };
  const db = path[0] ?? null;
  const schema = path.length > 1 && conn && SQL_KINDS.includes(conn.kind) ? path[1] : null;
  return (
    <Screen title={title} sub={path.length ? `${conn?.name ?? ""} › ${path.join(" › ")}` : conn ? KIND_LABEL[conn.kind] : undefined}
      right={<>
        {isRedis && db && <IconBtn label="New key" onClick={() => nav.push({ s: "rediskey", connId, database: db, key: null })}><Plus className="size-5" /></IconBtn>}
        {conn && conn.kind !== "redis" && <Btn size="sm" onClick={() => nav.push({ s: "query", connId, database: db, schema })}><Play className="size-3.5" />Query</Btn>}
        {isRedis && <Btn size="sm" onClick={() => nav.push({ s: "query", connId, database: db })}>Console</Btn>}
      </>}>
      {isRedis && path.length >= 1 ? (
        <div className="flex gap-2">
          <Input className="flex-1 font-mono" placeholder="Key prefix, e.g. user:" value={prefix} onChange={(e) => setPrefix(e.target.value)} />
          <Btn onClick={() => nav.push({ s: "browse", connId, path: [path[0], prefix], title: prefix || path[0] })} disabled={!prefix}><Search className="size-4" /></Btn>
        </div>
      ) : (nodes.data?.length ?? 0) > 8 && <Input placeholder="Filter" value={q} onChange={(e) => setQ(e.target.value)} />}
      {nodes.loading ? <Loading /> : nodes.error ? <ErrorBox error={nodes.error} onRetry={nodes.reload} /> : list.length === 0 ? <Empty title="Nothing here" /> : list.map((n) => (
        <Item key={n.path.join("\u0000") + n.label} onClick={() => open(n)} icon={nodeIcon(n)} title={n.label}
          right={n.detail ? <span className="max-w-[40%] truncate text-[11px] text-muted">{n.detail}</span> : undefined}
          chevron={n.hasChildren || !!n.table || n.kind === "key"} />
      ))}
    </Screen>
  );
}

// ------------------------------------------------------------------ rows

const PAGE = 50;

export function Rows({ connId, table }: { connId: string; table: TableRef }) {
  const conn = useConn(connId);
  const [offset, setOffset] = useState(0);
  const [filter, setFilter] = useState("");
  const [applied, setApplied] = useState<string | null>(null);
  const [sort, setSort] = useState<{ column: string; desc: boolean } | null>(null);
  const [sheet, setSheet] = useState(false);
  const page = useLoad(() => host<TableDataPage>("db_table_data", { connId, req: { table, offset, limit: PAGE, filter: applied, sort: sort ? [sort] : [] } }), [connId, table.name, offset, applied, sort?.column, sort?.desc]);
  const cols = page.data?.result.columns ?? [];
  const pk = page.data?.primaryKey ?? [];
  const total = page.data?.total;
  const isMongo = conn?.kind === "mongodb";
  const editable = !!conn && !conn.readOnly && (isMongo ? false : pk.length > 0);
  const shown = cols.filter((c) => !pk.includes(c.name)).slice(0, 5);
  const titleOf = (row: CellValue[]) => pk.length ? pk.map((k) => cellText(row[cols.findIndex((c) => c.name === k)])).join(" · ") : cellText(row[0]);

  return (
    <Screen title={table.name} sub={total != null ? `${total.toLocaleString()} rows${applied ? " match" : ""}` : [table.database, table.schema].filter(Boolean).join(" › ")}
      right={<>
        {editable && <IconBtn label="Add row" onClick={() => nav.push({ s: "row", connId, table, row: null, pk })}><Plus className="size-5" /></IconBtn>}
        <IconBtn label="Filter and sort" onClick={() => setSheet(true)}><Search className="size-4" /></IconBtn>
      </>}>
      {(applied || sort) && (
        <div className="flex flex-wrap gap-1.5">
          {applied && <button onClick={() => { setApplied(null); setFilter(""); setOffset(0); }} className="rounded-full bg-accent/15 px-2.5 py-1 font-mono text-[11.5px] text-accent">{applied} ✕</button>}
          {sort && <button onClick={() => setSort(null)} className="rounded-full bg-hover px-2.5 py-1 text-[11.5px] text-muted">Sort: {sort.column} {sort.desc ? "↓" : "↑"} ✕</button>}
        </div>
      )}
      {page.loading && !page.data ? <Loading /> : page.error ? <ErrorBox error={page.error} onRetry={page.reload} /> : (page.data?.result.rows.length ?? 0) === 0 ? <Empty title="No rows" /> : page.data!.result.rows.map((row, i) => {
        const obj = Object.fromEntries(cols.map((c, j) => [c.name, row[j]]));
        return (
          <Card key={offset + i} onClick={() => nav.push({ s: "row", connId, table, row: obj, pk })}>
            <div className="flex items-center gap-2"><b className="min-w-0 flex-1 truncate">{pk.length ? "#" : ""}{titleOf(row)}</b>{editable && <Pencil className="size-3.5 text-faint" />}</div>
            <div className="mt-1.5 grid grid-cols-[minmax(72px,auto)_1fr] gap-x-3 gap-y-0.5 text-[12px]">
              {shown.map((c) => {
                const v = row[cols.indexOf(c)];
                return [<span key={c.name + "k"} className="truncate text-muted">{c.name}</span>, <span key={c.name + "v"} className={clsx("truncate font-mono text-[11.5px]", v === null && "text-faint")}>{cellText(v)}</span>];
              })}
            </div>
          </Card>
        );
      })}
      {page.data && (
        <div className="flex items-center gap-2 pt-1">
          <span className="flex-1 text-[12px] text-muted">Rows {page.data.result.rows.length ? offset + 1 : 0}–{offset + page.data.result.rows.length}{total != null ? ` of ${total.toLocaleString()}` : ""}</span>
          <Btn size="sm" disabled={offset === 0} onClick={() => setOffset(Math.max(0, offset - PAGE))}>‹</Btn>
          <Btn size="sm" disabled={page.data.result.rows.length < PAGE || (total != null && offset + PAGE >= total)} onClick={() => setOffset(offset + PAGE)}>›</Btn>
        </div>
      )}
      <Sheet open={sheet} onClose={() => setSheet(false)} title="Filter and sort">
        <Field label={isMongo ? "Filter (JSON)" : "WHERE"} hint={isMongo ? '{ "status": "paid" }' : "status = 'delivered' AND total > 100"}>
          <TextArea rows={3} className="font-mono text-[12.5px]" value={filter} onChange={(e) => setFilter(e.target.value)} />
        </Field>
        <Field label="Sort by">
          <select className="h-11 w-full rounded-xl border border-line bg-panel px-3" value={sort?.column ?? ""} onChange={(e) => setSort(e.target.value ? { column: e.target.value, desc: sort?.desc ?? true } : null)}>
            <option value="">(default)</option>
            {cols.map((c) => <option key={c.name} value={c.name}>{c.name}</option>)}
          </select>
        </Field>
        {sort && <Seg value={sort.desc ? "desc" : "asc"} onChange={(v) => setSort({ ...sort, desc: v === "desc" })} options={[{ value: "desc", label: "Newest / largest first" }, { value: "asc", label: "Ascending" }]} />}
        <Btn variant="primary" block onClick={() => { setApplied(filter.trim() || null); setOffset(0); setSheet(false); }}>Apply</Btn>
      </Sheet>
    </Screen>
  );
}

// ------------------------------------------------------------------ one row

export function Row({ connId, table, row, pk }: { connId: string; table: TableRef; row: Record<string, CellValue> | null; pk: string[] }) {
  const conn = useConn(connId);
  const st = useLoad(() => host<TableStructure>("db_table_structure", { connId, table }), [connId, table.name]);
  const isMongo = conn?.kind === "mongodb";
  const editable = !!conn && !conn.readOnly && !isMongo && (pk.length > 0 || row === null);
  const [vals, setVals] = useState<Record<string, string | null>>({});
  const [busy, setBusy] = useState(false);
  const [del, setDel] = useState(false);
  const cols = st.data?.columns ?? (row ? Object.keys(row).map((name) => ({ name, dataType: "", nullable: true, isPrimaryKey: pk.includes(name), autoIncrement: false })) : []);
  const original = (c: string) => (row ? row[c] : undefined);
  const current = (c: string): string | null => (c in vals ? vals[c] : original(c) === undefined ? "" : original(c) === null ? null : cellText(original(c) as CellValue));
  const changed = Object.keys(vals).filter((c) => (vals[c] ?? null) !== (original(c) === null || original(c) === undefined ? (row ? null : "") : cellText(original(c) as CellValue)));
  const key = Object.fromEntries(pk.map((k) => [k, row?.[k] ?? null]));

  const apply = async (changes: RowChange[], done: string) => {
    setBusy(true);
    try {
      await host<number>("db_apply_changes", { connId, table, changes });
      toast(done, "ok");
      nav.back();
    } catch (e) {
      toastErr(e);
    } finally {
      setBusy(false);
    }
  };
  const save = () => {
    const values = Object.fromEntries(changed.map((c) => [c, vals[c]]));
    void apply(row ? [{ op: "update", key, values }] : [{ op: "insert", values }], row ? "Saved" : "Row added");
  };

  if (isMongo && row) {
    return (
      <Screen title={table.name} sub="Document">
        <pre className="selectable overflow-x-auto rounded-xl border border-line bg-panel p-3 font-mono text-[11.5px] whitespace-pre-wrap">{JSON.stringify(row, null, 2)}</pre>
        <Note>Edit documents with a query (e.g. <b>db.{table.name}.updateOne(…)</b>) from the Query screen.</Note>
      </Screen>
    );
  }

  return (
    <Screen title={row ? `${table.name} · ${pk.map((k) => cellText(row[k] ?? null)).join(" · ")}` : `New row in ${table.name}`}>
      {st.loading && <Loading />}
      {st.error && <ErrorBox error={st.error} />}
      {cols.map((c) => {
        const v = current(c.name);
        const locked = !editable || (row !== null && c.isPrimaryKey) || (row === null && c.autoIncrement);
        return (
          <Field key={c.name} label={<>{c.name}{c.dataType && <span className="text-faint"> · {c.dataType}</span>}{c.isPrimaryKey && <span className="text-faint"> · primary key</span>}{c.nullable && <span className="text-faint"> · nullable</span>}</>}>
            <div className="flex gap-2">
              {v !== null && v.length > 60 ? (
                <TextArea rows={4} className={clsx("flex-1 font-mono text-[12.5px]", changed.includes(c.name) && "border-accent")} disabled={locked} value={v} onChange={(e) => setVals({ ...vals, [c.name]: e.target.value })} />
              ) : (
                <Input className={clsx("flex-1 font-mono", changed.includes(c.name) && "border-accent", v === null && "text-faint")} disabled={locked} value={v ?? ""} placeholder={v === null ? "NULL" : row === null && c.autoIncrement ? "auto" : ""}
                  onChange={(e) => setVals({ ...vals, [c.name]: e.target.value })} />
              )}
              {!locked && c.nullable && <Btn size="sm" className="h-11" variant={v === null ? "primary" : "default"} onClick={() => setVals({ ...vals, [c.name]: v === null ? "" : null })}>NULL</Btn>}
            </div>
          </Field>
        );
      })}
      {!editable && row && <Note>{conn?.readOnly ? "This connection is read-only." : pk.length === 0 ? "This table has no primary key, so rows can't be edited here." : "Read-only."}</Note>}
      {editable && changed.length > 0 && <Note>{changed.length} change{changed.length === 1 ? "" : "s"}: {changed.join(", ")}</Note>}
      {editable && (
        <div className="flex gap-2 pt-1">
          {row && <Btn variant="danger" onClick={() => setDel(true)}><Trash2 className="size-4" />Delete</Btn>}
          <Btn variant="primary" className="flex-1" loading={busy} disabled={changed.length === 0} onClick={save}><Save className="size-4" />{row ? "Save change" : "Add row"}</Btn>
        </div>
      )}
      <Confirm open={del} danger title="Delete this row?" confirm="Delete" onCancel={() => setDel(false)} onConfirm={() => { setDel(false); void apply([{ op: "delete", key }], "Row deleted"); }}>
        It's removed from <b>{table.name}</b> right away.
      </Confirm>
    </Screen>
  );
}

// ------------------------------------------------------------------ query

/** Why a script needs confirming, and the word to type. */
export function dangerOf(script: string, kind?: ConnectionKind): { why: string; word: string } | null {
  if (kind === "redis") {
    const m = /^\s*(flushall|flushdb|del|unlink)\b(.*)$/im.exec(script);
    if (m) return { why: `${m[1].toUpperCase()} deletes data.`, word: m[1].toLowerCase().startsWith("flush") ? m[1].toUpperCase() : "delete" };
    return null;
  }
  if (kind === "mongodb") {
    const m = /db\.(\w+)\.(drop|deleteMany|remove)\s*\(\s*(\{\s*\})?/i.exec(script);
    if (m) return { why: `${m[2]} on ${m[1]}${m[3] || m[2] === "drop" ? " removes every document." : " deletes documents."}`, word: m[1] };
    return null;
  }
  for (const raw of script.replace(/--[^\n]*|\/\*[\s\S]*?\*\//g, " ").split(";")) {
    const s = raw.trim();
    let m = /^(drop|truncate)\s+(?:table\s+)?(?:if\s+exists\s+)?([\w."`[\]]+)/i.exec(s);
    if (m) return { why: `${m[1].toUpperCase()} ${m[2]} can't be undone.`, word: m[2].replace(/["`[\]]/g, "").split(".").pop()! };
    m = /^delete\s+from\s+([\w."`[\]]+)/i.exec(s);
    if (m && !/\bwhere\b/i.test(s)) return { why: `DELETE without WHERE on ${m[1]} removes every row.`, word: m[1].replace(/["`[\]]/g, "").split(".").pop()! };
    m = /^update\s+([\w."`[\]]+)\s+set\b/i.exec(s);
    if (m && !/\bwhere\b/i.test(s)) return { why: `UPDATE without WHERE changes every row of ${m[1]}.`, word: m[1].replace(/["`[\]]/g, "").split(".").pop()! };
  }
  return null;
}

export function Query({ connId, database, schema, sql }: { connId: string; database?: string | null; schema?: string | null; sql?: string }) {
  const conn = useConn(connId);
  const [text, setText] = useState(sql ?? "");
  const [results, setResults] = useState<QueryResult[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [view, setView] = useState<"table" | "cards">("table");
  const [confirm, setConfirm] = useState<{ why: string; word: string } | null>(null);
  const [pick, setPick] = useState<"history" | "saved" | null>(null);
  const history = useLoad(() => (pick === "history" ? host<HistoryEntry[]>("history_list", { connectionId: connId, search: null, limit: 50 }) : Promise.resolve([])), [pick]);
  const saved = useLoad(() => (pick === "saved" ? host<SavedQuery[]>("saved_queries_list") : Promise.resolve([])), [pick]);

  const run = async (force = false) => {
    if (!text.trim()) return;
    const d = dangerOf(text, conn?.kind);
    if (d && !force) { setConfirm(d); return; }
    setBusy(true);
    setError(null);
    try {
      setResults(await host<QueryResult[]>("db_execute", { connId, script: text, opts: { database: database ?? null, schema: schema ?? null, maxRows: 500 } }));
    } catch (e) {
      setError(typeof e === "string" ? e : JSON.stringify(e));
      setResults(null);
    } finally {
      setBusy(false);
    }
  };
  const cancel = () => void host("db_cancel", { connId }).catch(() => {});

  return (
    <Screen title={conn?.kind === "redis" ? "Console" : "Query"} sub={[conn?.name, database, schema].filter(Boolean).join(" · ")}>
      <TextArea rows={7} className="font-mono text-[12.5px] leading-relaxed" placeholder={conn?.kind === "redis" ? "GET key\nHGETALL user:1" : conn?.kind === "mongodb" ? "db.orders.find({ status: 'paid' }).limit(20)" : "SELECT * FROM orders LIMIT 20;"} value={text} onChange={(e) => setText(e.target.value)} />
      <div className="flex gap-2">
        <Btn size="sm" onClick={() => setPick("history")}><HistoryIcon className="size-3.5" />History</Btn>
        <Btn size="sm" onClick={() => setPick("saved")}>Saved</Btn>
        <span className="flex-1" />
        {busy ? <Btn size="sm" variant="danger" onClick={cancel}>Cancel</Btn> : null}
        <Btn size="sm" variant="primary" loading={busy} onClick={() => void run()}><Play className="size-3.5" />Run</Btn>
      </div>
      {error && <ErrorBox error={error} />}
      {results?.map((r, i) => (
        <div key={i} className="space-y-1.5">
          <div className="flex items-center gap-2 text-[12px] text-muted">
            <span className="flex-1">{r.columns.length ? `${r.rows.length}${r.truncated ? "+" : ""} rows` : r.rowsAffected != null ? `${r.rowsAffected} rows affected` : r.message ?? "Done"} · {r.elapsedMs} ms</span>
            {i === 0 && results.some((x) => x.columns.length) && <div className="w-32"><Seg value={view} onChange={setView} options={[{ value: "table", label: "Table" }, { value: "cards", label: "Cards" }]} /></div>}
          </div>
          {r.columns.length > 0 && (view === "table" ? (
            <div className="overflow-x-auto rounded-xl border border-line">
              <table className="selectable w-max min-w-full text-[11.5px]">
                <thead><tr>{r.columns.map((c) => <th key={c.name} className="border-b border-line bg-panel px-2 py-1.5 text-left font-semibold text-muted">{c.name}</th>)}</tr></thead>
                <tbody>{r.rows.map((row, j) => <tr key={j}>{row.map((v, k) => <td key={k} className={clsx("max-w-64 truncate border-b border-line px-2 py-1.5 font-mono", v === null && "text-faint")}>{cellText(v)}</td>)}</tr>)}</tbody>
              </table>
            </div>
          ) : r.rows.map((row, j) => (
            <Card key={j}>
              <div className="grid grid-cols-[minmax(72px,auto)_1fr] gap-x-3 gap-y-0.5 text-[12px]">
                {r.columns.map((c, k) => [<span key={k + "k"} className="truncate text-muted">{c.name}</span>, <span key={k + "v"} className="selectable font-mono text-[11.5px] break-all">{cellText(row[k])}</span>])}
              </div>
            </Card>
          )))}
        </div>
      ))}
      <Confirm open={!!confirm} danger title="⚠ This changes data" confirm="Run anyway" typeToConfirm={confirm?.word}
        onCancel={() => setConfirm(null)} onConfirm={() => { setConfirm(null); void run(true); }}>{confirm?.why}</Confirm>
      <Sheet open={pick !== null} onClose={() => setPick(null)} title={pick === "history" ? "History" : "Saved queries"}>
        {(pick === "history" ? history : saved).loading ? <Loading /> : null}
        {pick === "history" && history.data?.map((h) => (
          <Item key={h.id} onClick={() => { setText(h.query); setPick(null); }} title={<span className="font-mono text-[12px]">{h.query.replace(/\s+/g, " ").slice(0, 120)}</span>} meta={`${ago(h.executedAt)} · ${h.elapsedMs} ms${h.success ? "" : " · failed"}`} />
        ))}
        {pick === "saved" && saved.data?.filter((s) => !s.connectionId || s.connectionId === connId).map((s) => (
          <Item key={s.id} onClick={() => { setText(s.query); setPick(null); }} title={s.name} meta={<span className="font-mono">{s.query.replace(/\s+/g, " ").slice(0, 80)}</span>} />
        ))}
        {pick === "history" && history.data?.length === 0 && <Empty title="No history yet" />}
        {pick === "saved" && saved.data?.length === 0 && <Empty title="No saved queries" />}
      </Sheet>
    </Screen>
  );
}

// ------------------------------------------------------------------ redis key

type RedisValue = { key: string; type: string; ttl: number; value: any; length?: number; binary?: boolean };
const REDIS_TYPES = ["string", "hash", "list", "set", "zset"] as const;

export function RedisKey({ connId, database, keyName: key }: { connId: string; database: string | null; keyName: string | null }) {
  const isNew = key === null;
  const loaded = useLoad(() => (isNew ? Promise.resolve<RedisValue>({ key: "", type: "string", ttl: -1, value: "" }) : host<RedisValue>("db_get_key", { connId, database, key })), [connId, database, key]);
  const [v, setV] = useState<RedisValue | null>(null);
  const [busy, setBusy] = useState(false);
  const [del, setDel] = useState(false);
  useEffect(() => { if (loaded.data) setV(structuredClone(loaded.data)); }, [loaded.data]);
  if (loaded.loading || !v) return <Screen title={key ?? "New key"}>{loaded.error ? <ErrorBox error={loaded.error} /> : <Loading />}</Screen>;
  const readOnlyType = v.type === "stream" || v.type === "none" || v.binary;
  const set = (patch: Partial<RedisValue>) => setV({ ...v, ...patch });

  const save = async () => {
    setBusy(true);
    try {
      await host("db_set_key", { connId, database, value: { key: v.key, oldKey: isNew ? null : key, type: v.type, ttl: v.ttl > 0 ? v.ttl : -1, value: v.value, length: v.length } });
      toast("Saved", "ok");
      if (isNew) nav.back(); else loaded.reload();
    } catch (e) {
      toastErr(e);
    } finally {
      setBusy(false);
    }
  };
  const remove = async () => {
    try {
      await host("db_delete_key", { connId, database, key });
      toast("Key deleted", "ok");
      nav.back();
    } catch (e) {
      toastErr(e);
    }
  };

  const listEditor = (items: any[], render: (item: any, i: number) => ReactNode, add: () => void) => (
    <div className="space-y-1.5">
      {items.map((it, i) => (
        <div key={i} className="flex items-start gap-2">
          <div className="min-w-0 flex-1">{render(it, i)}</div>
          <IconBtn label="Remove" onClick={() => set({ value: items.filter((_, j) => j !== i) })}><Trash2 className="size-4" /></IconBtn>
        </div>
      ))}
      <Btn size="sm" onClick={add}><Plus className="size-3.5" />Add</Btn>
    </div>
  );
  const pretty = (s: string) => { try { const j = JSON.parse(s); return typeof j === "object" && j !== null ? JSON.stringify(j, null, 2) : s; } catch { return s; } };

  return (
    <Screen title={isNew ? "New key" : v.key} sub={`${v.type}${v.length != null ? ` · ${v.length} items` : ""}${database ? ` · ${database}` : ""}`}>
      {isNew && (
        <>
          <Field label="Key name"><Input className="font-mono" value={v.key} onChange={(e) => set({ key: e.target.value })} /></Field>
          <Field label="Type">
            <Seg value={v.type as (typeof REDIS_TYPES)[number]} onChange={(t) => set({ type: t, value: t === "string" ? "" : t === "hash" ? {} : [] })} options={REDIS_TYPES.map((t) => ({ value: t, label: t }))} />
          </Field>
        </>
      )}
      <Field label="TTL in seconds" hint="Empty = never expires">
        <Input inputMode="numeric" value={v.ttl > 0 ? String(v.ttl) : ""} placeholder="—" onChange={(e) => set({ ttl: Number(e.target.value) || -1 })} disabled={readOnlyType} />
      </Field>
      <Section>Value</Section>
      {v.type === "string" && <TextArea rows={10} className="font-mono text-[12.5px]" value={pretty(String(v.value ?? ""))} onChange={(e) => set({ value: e.target.value })} disabled={readOnlyType} />}
      {v.type === "hash" && listEditor(Object.entries(v.value ?? {}), ([f, val]: [string, any], i) => (
        <div className="space-y-1">
          <Input className="h-9 font-mono text-[12.5px]" value={f} onChange={(e) => { const entries = Object.entries(v.value); entries[i] = [e.target.value, val]; set({ value: Object.fromEntries(entries) }); }} />
          <Input className="h-9 font-mono text-[12.5px]" value={String(val ?? "")} onChange={(e) => set({ value: { ...v.value, [f]: e.target.value } })} />
        </div>
      ), () => set({ value: { ...v.value, [`field${Object.keys(v.value ?? {}).length + 1}`]: "" } }))}
      {(v.type === "list" || v.type === "set") && listEditor(v.value ?? [], (it, i) => (
        <Input className="h-9 font-mono text-[12.5px]" value={String(it ?? "")} onChange={(e) => { const a = [...v.value]; a[i] = e.target.value; set({ value: a }); }} />
      ), () => set({ value: [...(v.value ?? []), ""] }))}
      {v.type === "zset" && listEditor(v.value ?? [], (it: [string, number], i) => (
        <div className="flex gap-1.5">
          <Input className="h-9 flex-1 font-mono text-[12.5px]" value={String(it[0] ?? "")} onChange={(e) => { const a = [...v.value]; a[i] = [e.target.value, it[1]]; set({ value: a }); }} />
          <Input className="h-9 w-24 font-mono text-[12.5px]" inputMode="decimal" value={String(it[1] ?? 0)} onChange={(e) => { const a = [...v.value]; a[i] = [it[0], e.target.value]; set({ value: a }); }} />
        </div>
      ), () => set({ value: [...(v.value ?? []), ["", 0]] }))}
      {v.type === "stream" && (v.value as any[]).map((e) => (
        <Card key={e.id}><div className="font-mono text-[11px] text-muted">{e.id}</div><pre className="selectable font-mono text-[11.5px] whitespace-pre-wrap">{JSON.stringify(e.fields, null, 1)}</pre></Card>
      ))}
      {!["string", "hash", "list", "set", "zset", "stream"].includes(v.type) && <pre className="selectable rounded-xl bg-panel p-3 font-mono text-[12px] whitespace-pre-wrap">{JSON.stringify(v.value, null, 2)}</pre>}
      {v.binary && <Note>This value isn't text; edit it on the computer.</Note>}
      {(v.length ?? 0) > (Array.isArray(v.value) ? v.value.length : Object.keys(v.value ?? {}).length) && <Note>Only the first items are shown; large keys can't be saved from here.</Note>}
      <div className="flex gap-2 pt-1">
        {!isNew && <Btn variant="danger" onClick={() => setDel(true)}><Trash2 className="size-4" />Delete key</Btn>}
        {!readOnlyType && <Btn variant="primary" className="flex-1" loading={busy} disabled={!v.key} onClick={() => void save()}><Save className="size-4" />Save</Btn>}
      </div>
      <Confirm open={del} danger title={`Delete ${key}?`} confirm="Delete" onCancel={() => setDel(false)} onConfirm={() => { setDel(false); void remove(); }}>The key and its value are removed from Redis.</Confirm>
    </Screen>
  );
}

