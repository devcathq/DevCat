import MiniSelect from "../grid/MiniSelect";
import { AlignLeft, Bot, ChevronDown, Gauge, ListTree, Play, PlaySquare, Save, Square } from "lucide-react";
import type { ReactNode } from "react";
import { Button, IconButton } from "../../components/ui";
import { KindIcon } from "../../components/ui/KindIcon";
import { DATABASE_KINDS, type ConnectionKind } from "../../lib/types";
import { useConnections, useScopedConnections } from "../../store/connections";
import type { Tab } from "../../tabs/types";
import { useContextMenu } from "../connections/ContextMenu";
import { hasSchemas, isSqlKind } from "../table/sqlDialect";
import { openConnectionAgent } from "../agent/connection";

export type ExplainMode = "plain" | "analyze";

const MAX_ROWS: (number | null)[] = [100, 500, 1000, 5000, 10000, 50000, 100000, null];
const isMac = typeof navigator !== "undefined" && /Mac/.test(navigator.platform);
const MOD = isMac ? "⌘" : "Ctrl+";

interface Props {
  tab: Extract<Tab, { type: "query" }>;
  kind: ConnectionKind;
  databases: string[];
  schemas: string[];
  metaError: string | null;
  running: boolean;
  maxRows: number | null;
  onMaxRows: (n: number | null) => void;
  onConnection: (id: string) => void;
  onDatabase: (db: string | null) => void;
  onSchema: (s: string | null) => void;
  onRun: () => void;
  onRunAll: () => void;
  onStop: () => void;
  onExplain: (m: ExplainMode) => void;
  onFormat: () => void;
  onSave: () => void;
  onSaveAs: () => void;
}

export default function QueryToolbar(p: Props) {
  const connections = useScopedConnections();
  const current = useConnections((s) => s.connections.find((c) => c.id === p.tab.connId));
  // Focus mode limits the picker to the project, but always keep the tab's own connection.
  const scoped = current && !connections.includes(current) ? [current, ...connections] : connections;
  const dbConns = scoped.filter((c) => DATABASE_KINDS.includes(c.kind));
  const menu = useContextMenu();
  const canExplain = p.kind !== "redis" && p.kind !== "mssql";
  const hasAnalyze = p.kind === "postgres" || p.kind === "mysql" || p.kind === "mongodb";
  const dbs = p.tab.database && !p.databases.includes(p.tab.database) ? [p.tab.database, ...p.databases] : p.databases;
  const schemas = p.tab.schema && !p.schemas.includes(p.tab.schema) ? [p.tab.schema, ...p.schemas] : p.schemas;

  const dropdown = (e: React.MouseEvent, items: Parameters<typeof menu.show>[1]) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    menu.show({ clientX: r.left, clientY: r.bottom + 2, preventDefault: () => {} }, items);
  };

  return (
    <div className="flex h-9 shrink-0 items-center gap-1 border-b border-line bg-panel px-2">
      <Labeled icon={<KindIcon kind={p.kind} className="size-3.5" />}>
        <MiniSelect className="w-40 pl-6" value={p.tab.connId} onChange={(e) => p.onConnection(e.target.value)} title="Connection">
          {dbConns.map((c) => <option key={c.id} value={c.id}>{c.name}</option>)}
        </MiniSelect>
      </Labeled>
      {p.kind !== "sqlite" && (
        <MiniSelect
          className="w-36"
          value={p.tab.database ?? ""}
          onChange={(e) => p.onDatabase(e.target.value || null)}
          title={p.metaError ? `Could not list databases: ${p.metaError}` : p.kind === "redis" ? "Database index" : "Database"}
        >
          {!p.tab.database && <option value="">{p.metaError ? "(unavailable)" : "Database…"}</option>}
          {dbs.map((d) => <option key={d} value={d}>{p.kind === "redis" && /^\d+$/.test(d) ? `db${d}` : d}</option>)}
        </MiniSelect>
      )}
      {hasSchemas(p.kind) && (
        <MiniSelect className="w-32" value={p.tab.schema ?? ""} onChange={(e) => p.onSchema(e.target.value || null)} title="Schema">
          {!p.tab.schema && <option value="">Schema…</option>}
          {schemas.map((s) => <option key={s} value={s}>{s}</option>)}
        </MiniSelect>
      )}

      <div className="mx-1 h-4 w-px bg-line" />

      <Button size="sm" variant="primary" onClick={p.onRun} disabled={p.running} title={`Run statement under cursor or selection (${MOD}↵)`}>
        <Play className="size-3.5 fill-current" /> Run
      </Button>
      <IconButton title={`Run all (${MOD}⇧↵)`} onClick={p.onRunAll} disabled={p.running}><PlaySquare className="size-4" /></IconButton>
      <IconButton title="Stop" onClick={p.onStop} disabled={!p.running} className={p.running ? "text-danger" : ""}>
        <Square className="size-3.5 fill-current" />
      </IconButton>
      {canExplain && (
        <Button
          size="sm"
          variant="ghost"
          disabled={p.running}
          title={`Explain (${MOD}E)`}
          onClick={(e) => hasAnalyze
            ? dropdown(e, [
                { label: "Explain", shortcut: `${MOD}E`, icon: <ListTree className="size-3.5" />, onClick: () => p.onExplain("plain") },
                { label: p.kind === "mongodb" ? "Explain (executionStats)" : "Explain Analyze", icon: <Gauge className="size-3.5" />, onClick: () => p.onExplain("analyze") },
              ])
            : p.onExplain("plain")}
        >
          <ListTree className="size-3.5" /> Explain {hasAnalyze && <ChevronDown className="size-3" />}
        </Button>
      )}
      {isSqlKind(p.kind) && (
        <Button size="sm" variant="ghost" onClick={p.onFormat} title="Format SQL (⇧⌥F)">
          <AlignLeft className="size-3.5" /> Format
        </Button>
      )}
      <div className="flex items-center">
        <Button size="sm" variant="ghost" onClick={p.onSave} title={`Save to saved queries (${MOD}S)`} className="rounded-r-none pr-1.5">
          <Save className="size-3.5" /> Save
        </Button>
        <Button
          size="sm"
          variant="ghost"
          className="rounded-l-none px-1"
          aria-label="More save options"
          onClick={(e) => dropdown(e, [
            { label: "Save", shortcut: `${MOD}S`, onClick: p.onSave },
            { label: "Save as new…", onClick: p.onSaveAs },
          ])}
        >
          <ChevronDown className="size-3" />
        </Button>
      </div>

      {current && (
        <Button size="sm" variant="ghost" onClick={() => void openConnectionAgent(current)} title={`Ask DevCat Agent about ${current.name}: it knows the tables, and can query, dump and export`}>
          <Bot className="size-3.5" /> Ask agent
        </Button>
      )}

      <div className="flex-1" />
      {p.kind !== "redis" && (
        <label className="flex items-center gap-1.5 whitespace-nowrap text-[11px] text-muted" title="Maximum rows fetched per result set">
          Max rows
          <MiniSelect
            className="w-24"
            value={p.maxRows === null ? "" : String(p.maxRows)}
            onChange={(e) => p.onMaxRows(e.target.value ? Number(e.target.value) : null)}
          >
            {MAX_ROWS.map((n) => <option key={n ?? "all"} value={n === null ? "" : n}>{n === null ? "Unlimited" : n.toLocaleString()}</option>)}
          </MiniSelect>
        </label>
      )}
      {menu.element}
    </div>
  );
}

function Labeled({ icon, children }: { icon: ReactNode; children: ReactNode }) {
  return (
    <div className="relative">
      <span className="pointer-events-none absolute top-1/2 left-1.5 -translate-y-1/2">{icon}</span>
      {children}
    </div>
  );
}
