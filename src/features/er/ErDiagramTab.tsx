import {
  Background, BackgroundVariant, Controls, MiniMap, ReactFlow, ReactFlowProvider, getNodesBounds, getViewportForBounds,
  useNodesState, useReactFlow, type Edge, type NodeMouseHandler,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { toPng, toSvg } from "html-to-image";
import {
  ArrowDownUp, ArrowLeftRight, Download, Filter, Image as ImageIcon, LayoutGrid, Maximize, RefreshCw, Search, Workflow, X,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import { Button, Empty, ErrorBox, IconButton, Input, Spinner, Toolbar, Spin } from "../../components/ui";
import { api, errMsg } from "../../lib/api";
import type { SchemaSnapshot, TableStructure } from "../../lib/types";
import type { Tab } from "../../tabs/types";
import { palette } from "../appearance/store";
import { openTableDesign } from "../connections/openers";
import { dataUrlToBytes, saveContent } from "../transfer/files";
import { TableChecklist, lsGet, lsSet, safeFileName } from "../transfer/shared";
import { autoLayout, type Direction } from "./layout";
import { TableNode, estimateSize, handleId, type TableNodeData, type TableNodeType } from "./TableNode";

type ErTab = Extract<Tab, { type: "er-diagram" }>;

interface Saved { dir: Direction; positions: Record<string, { x: number; y: number }>; hidden?: string[] }

const nodeTypes = { table: TableNode };
const tableKey = (t: TableStructure) => `${t.table?.schema ?? ""}.${t.table?.name ?? ""}`;

export default function ErDiagramTab({ tab }: { tab: ErTab }) {
  return (
    <ReactFlowProvider>
      <ErDiagram tab={tab} />
    </ReactFlowProvider>
  );
}

function ErDiagram({ tab }: { tab: ErTab }) {
  const storeKey = `er:${tab.connId}:${tab.database ?? ""}:${tab.schema ?? ""}`;
  const [snapshot, setSnapshot] = useState<SchemaSnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState<Saved>(() => lsGet<Saved>(storeKey, { dir: "LR", positions: {} }));
  const [hidden, setHidden] = useState<Set<string>>(() => new Set(saved.hidden ?? []));
  const [focus, setFocus] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const [hoverEdge, setHoverEdge] = useState<string | null>(null);
  const [showFilter, setShowFilter] = useState(false);
  const [nodes, setNodes, onNodesChange] = useNodesState<TableNodeType>([]);
  const [buildSeq, setBuildSeq] = useState(0);
  const rf = useReactFlow();
  const wrapRef = useRef<HTMLDivElement>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setSnapshot(await api.schemaSnapshot(tab.connId, tab.database ?? null, tab.schema ?? null));
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setLoading(false);
    }
  }, [tab.connId, tab.database, tab.schema]);
  useEffect(() => { load(); }, [load]);

  const persist = useCallback((patch: Partial<Saved>) => {
    setSaved((s) => {
      const next = { ...s, ...patch };
      lsSet(storeKey, next);
      return next;
    });
  }, [storeKey]);

  // Lookup structures
  const model = useMemo(() => {
    const tables = snapshot?.tables ?? [];
    const byKey = new Map<string, TableStructure>();
    const byName = new Map<string, string>();
    const multiSchema = new Set(tables.map((t) => t.table?.schema ?? "")).size > 1;
    for (const t of tables) {
      byKey.set(tableKey(t), t);
      if (!byName.has((t.table?.name ?? "").toLowerCase())) byName.set((t.table?.name ?? "").toLowerCase(), tableKey(t));
    }
    const resolve = (schema: string | null | undefined, name: string, fromSchema: string | null | undefined) =>
      byKey.has(`${schema ?? fromSchema ?? ""}.${name}`) ? `${schema ?? fromSchema ?? ""}.${name}` : byName.get(name.toLowerCase());
    const rels: { id: string; source: string; target: string; sourceCol: string; targetCol: string; name: string }[] = [];
    for (const t of tables) {
      t.foreignKeys.forEach((fk, i) => {
        const target = resolve(fk.refSchema, fk.refTable, t.table?.schema);
        const tt = target ? byKey.get(target) : undefined;
        const sourceCol = fk.columns.find((c) => t.columns.some((x) => x.name === c));
        if (!target || !tt || !sourceCol || !tt.columns.length) return;
        // Handles exist only for real columns; fall back to the PK / first column.
        const targetCol = fk.refColumns.find((c) => tt.columns.some((x) => x.name === c))
          ?? tt.columns.find((x) => x.isPrimaryKey)?.name ?? tt.columns[0].name;
        rels.push({
          id: `fk:${tableKey(t)}:${fk.name}:${i}`,
          source: tableKey(t), target,
          sourceCol, targetCol,
          name: `${fk.name || "fk"} (${fk.columns.join(", ")} → ${fk.refTable}.${fk.refColumns.join(", ")})`,
        });
      });
    }
    const label = (t: TableStructure) => (multiSchema && t.table?.schema ? `${t.table.schema}.${t.table.name}` : t.table?.name ?? "?");
    return { tables, byKey, rels, label };
  }, [snapshot]);

  const visibleTables = useMemo(() => model.tables.filter((t) => !hidden.has(tableKey(t))), [model, hidden]);

  // Build nodes whenever the table set / direction changes.
  const buildNodes = useCallback((reset: boolean) => {
    const sizes = new Map(visibleTables.map((t) => [tableKey(t), estimateSize(t, model.label(t))]));
    const layout = autoLayout(
      visibleTables.map((t) => ({ id: tableKey(t), ...sizes.get(tableKey(t))! })),
      model.rels.map((r) => ({ source: r.target, target: r.source })),
      saved.dir,
    );
    const positions = reset ? {} : saved.positions;
    setNodes(visibleTables.map((t) => {
      const id = tableKey(t);
      return {
        id,
        type: "table",
        position: positions[id] ?? layout.get(id) ?? { x: 0, y: 0 },
        data: {
          table: t,
          label: model.label(t),
          width: sizes.get(id)!.width,
          fkCols: new Set(t.foreignKeys.flatMap((f) => f.columns)),
          state: "normal",
        } satisfies TableNodeData,
      };
    }));
    setBuildSeq((n) => n + 1);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [visibleTables, model, saved.dir, setNodes]);

  const builtFor = useRef<unknown>(null);
  useEffect(() => {
    if (!snapshot) return;
    const first = builtFor.current !== snapshot;
    builtFor.current = snapshot;
    buildNodes(false);
    if (first) setTimeout(() => rf.fitView({ padding: 0.1, duration: 0, maxZoom: 1 }), 50);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [snapshot, visibleTables, saved.dir]);

  // Related set for focus highlighting
  const related = useMemo(() => {
    if (!focus) return null;
    const s = new Set<string>([focus]);
    for (const r of model.rels) {
      if (r.source === focus) s.add(r.target);
      if (r.target === focus) s.add(r.source);
    }
    return s;
  }, [focus, model.rels]);

  const matches = useMemo(() => {
    const q = search.trim().toLowerCase();
    if (!q) return null;
    return new Set(visibleTables.filter((t) => model.label(t).toLowerCase().includes(q)).map(tableKey));
  }, [search, visibleTables, model]);

  // Apply visual state without rebuilding positions
  useEffect(() => {
    setNodes((ns) => ns.map((n) => {
      let state: TableNodeData["state"] = "normal";
      if (related) state = n.id === focus ? "focus" : related.has(n.id) ? "related" : "dim";
      else if (matches) state = matches.has(n.id) ? "match" : "dim";
      return n.data.state === state ? n : { ...n, data: { ...n.data, state } };
    }));
  }, [related, matches, focus, setNodes, buildSeq]);

  const visibleIds = useMemo(() => new Set(visibleTables.map(tableKey)), [visibleTables]);
  const edges: Edge[] = useMemo(() => model.rels
    .filter((r) => visibleIds.has(r.source) && visibleIds.has(r.target))
    .map((r) => {
      const active = focus ? r.source === focus || r.target === focus : false;
      const dim = (focus && !active) || (!!matches && !(matches.has(r.source) || matches.has(r.target)));
      const hovered = hoverEdge === r.id;
      return {
        id: r.id,
        source: r.source,
        target: r.target,
        sourceHandle: handleId(r.sourceCol, "s"),
        targetHandle: handleId(r.targetCol, "t"),
        type: "smoothstep",
        animated: active,
        label: hovered ? r.name : undefined,
        labelStyle: { fill: "var(--color-fg)", fontSize: 11 },
        labelBgStyle: { fill: "var(--color-raised)" },
        labelBgPadding: [6, 3] as [number, number],
        labelBgBorderRadius: 4,
        zIndex: hovered || active ? 10 : 0,
        style: {
          stroke: active || hovered ? "var(--color-accent)" : "var(--color-faint)",
          strokeWidth: active || hovered ? 1.8 : 1.2,
          opacity: dim ? 0.12 : 1,
        },
      } satisfies Edge;
    }), [model.rels, visibleIds, focus, matches, hoverEdge]);

  const onNodeClick: NodeMouseHandler<TableNodeType> = (_, n) => setFocus((f) => (f === n.id ? null : n.id));
  const onNodeDoubleClick: NodeMouseHandler<TableNodeType> = (_, n) => {
    const t = n.data.table.table;
    if (t) openTableDesign(tab.connId, { ...t, database: t.database ?? tab.database ?? null, schema: t.schema ?? tab.schema ?? null });
  };
  const onNodeDragStop = useCallback((_: unknown, __: unknown, dragged: TableNodeType[]) => {
    const positions = { ...saved.positions };
    for (const n of dragged) positions[n.id] = n.position;
    persist({ positions });
  }, [saved.positions, persist]);

  const relayout = () => {
    persist({ positions: {} });
    buildNodes(true);
    setTimeout(() => rf.fitView({ padding: 0.1, duration: 300, maxZoom: 1 }), 50);
  };
  const setDir = (dir: Direction) => {
    persist({ dir, positions: {} });
    setTimeout(() => rf.fitView({ padding: 0.1, duration: 300, maxZoom: 1 }), 80);
  };

  const jumpToMatch = () => {
    if (!matches?.size) return;
    const ns = nodes.filter((n) => matches.has(n.id));
    if (ns.length === 1) setFocus(ns[0].id);
    rf.fitView({ nodes: ns.map((n) => ({ id: n.id })), padding: 0.3, duration: 400, maxZoom: 1.2 });
  };

  const setHiddenPersist = (h: Set<string>) => {
    setHidden(h);
    persist({ hidden: [...h] });
  };

  // Export
  const [exporting, setExporting] = useState(false);
  const exportImage = async (kind: "png" | "svg") => {
    const viewport = wrapRef.current?.querySelector<HTMLElement>(".react-flow__viewport");
    if (!viewport || !nodes.length) return;
    setExporting(true);
    try {
      const bounds = getNodesBounds(nodes);
      const pad = 40;
      const width = Math.min(8000, Math.ceil(bounds.width + pad * 2));
      const height = Math.min(8000, Math.ceil(bounds.height + pad * 2));
      const vp = getViewportForBounds(bounds, width, height, 0.05, 2, 0.02);
      const opts = {
        backgroundColor: palette().panel,
        width, height,
        style: { width: `${width}px`, height: `${height}px`, transform: `translate(${vp.x}px, ${vp.y}px) scale(${vp.zoom})` },
        filter: (el: HTMLElement) => !el.classList?.contains("react-flow__minimap"),
      };
      const base = safeFileName(`er-${tab.database ?? "db"}${tab.schema ? "-" + tab.schema : ""}`);
      if (kind === "png") {
        const url = await toPng(viewport, { ...opts, pixelRatio: width * height > 12e6 ? 1 : 2 });
        const p = await saveContent({ defaultName: `${base}.png`, filters: [{ name: "PNG image", extensions: ["png"] }], content: dataUrlToBytes(url), dirKey: "er-export", mime: "image/png" });
        if (p) toast.success(`Diagram exported${p === "download" ? "" : ` to ${p}`}`);
      } else {
        const url = await toSvg(viewport, opts);
        const svg = decodeURIComponent(url.slice(url.indexOf(",") + 1));
        const p = await saveContent({ defaultName: `${base}.svg`, filters: [{ name: "SVG image", extensions: ["svg"] }], content: svg, dirKey: "er-export", mime: "image/svg+xml" });
        if (p) toast.success(`Diagram exported${p === "download" ? "" : ` to ${p}`}`);
      }
    } catch (e) {
      toast.error(`Export failed: ${errMsg(e)}`);
    } finally {
      setExporting(false);
    }
  };

  const focusInfo = focus ? model.byKey.get(focus) : null;
  const tableRefs = useMemo(() => model.tables.map((t) => ({ name: t.table?.name ?? "", schema: t.table?.schema ?? "", database: t.table?.database })), [model]);
  const refKey = useCallback((t: { schema?: string | null; name: string }) => `${t.schema ?? ""}.${t.name}`, []);
  const refLabel = useCallback((t: { schema?: string | null; name: string }) => model.label({ table: t } as TableStructure), [model]);
  const shownSet = useMemo(() => new Set(model.tables.map(tableKey).filter((k) => !hidden.has(k))), [model, hidden]);

  return (
    <div className="flex h-full flex-col">
      <Toolbar>
        <Workflow className="ml-1 size-4 text-accent" />
        <span className="mr-2 text-[13px] font-medium">{[tab.database, tab.schema].filter(Boolean).join(" / ") || "ER Diagram"}</span>
        <div className="relative w-56">
          <Search className="pointer-events-none absolute left-2 top-1.5 size-3.5 text-faint" />
          <Input
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") jumpToMatch();
              if (e.key === "Escape") setSearch("");
            }}
            placeholder="Find table (Enter to zoom)"
            className="h-6 pl-7 text-xs"
          />
        </div>
        {matches && <span className="text-[11px] text-muted">{matches.size} match{matches.size === 1 ? "" : "es"}</span>}
        <div className="mx-1 h-4 w-px bg-line" />
        <IconButton title="Left → right layout" active={saved.dir === "LR"} onClick={() => setDir("LR")}><ArrowLeftRight className="size-3.5" /></IconButton>
        <IconButton title="Top → bottom layout" active={saved.dir === "TB"} onClick={() => setDir("TB")}><ArrowDownUp className="size-3.5" /></IconButton>
        <IconButton title="Re-run auto layout (resets dragged positions)" onClick={relayout}><LayoutGrid className="size-3.5" /></IconButton>
        <IconButton title="Zoom to fit" onClick={() => rf.fitView({ padding: 0.1, duration: 300 })}><Maximize className="size-3.5" /></IconButton>
        <IconButton title="Choose tables" active={showFilter} onClick={() => setShowFilter((v) => !v)}><Filter className="size-3.5" /></IconButton>
        <div className="flex-1" />
        <span className="text-[11px] text-faint">{visibleTables.length}/{model.tables.length} tables · {edges.length} relations</span>
        <Button size="sm" variant="ghost" onClick={() => exportImage("png")} disabled={exporting || !nodes.length}><ImageIcon className="size-3.5" />PNG</Button>
        <Button size="sm" variant="ghost" onClick={() => exportImage("svg")} disabled={exporting || !nodes.length}><Download className="size-3.5" />SVG</Button>
        <IconButton title="Reload schema" onClick={load}><Spin icon={RefreshCw} spinning={loading} className="size-3.5" /></IconButton>
      </Toolbar>
      <div className="relative flex min-h-0 flex-1">
        {showFilter && (
          <div className="flex w-60 shrink-0 flex-col gap-2 border-r border-line bg-panel p-2">
            <div className="flex items-center justify-between text-[11px] font-semibold uppercase tracking-wider text-muted">
              Tables
              <IconButton title="Close" onClick={() => setShowFilter(false)}><X className="size-3.5" /></IconButton>
            </div>
            <TableChecklist
              className="flex-1"
              tables={tableRefs}
              keyOf={refKey}
              labelOf={refLabel}
              selected={shownSet}
              onChange={(s) => setHiddenPersist(new Set(model.tables.map(tableKey).filter((k) => !s.has(k))))}
            />
            <Button
              size="sm"
              disabled={!focus}
              onClick={() => related && setHiddenPersist(new Set(model.tables.map(tableKey).filter((k) => !related.has(k))))}
              title="Show only the selected table and its directly related tables"
            >
              Only selected + related
            </Button>
          </div>
        )}
        <div ref={wrapRef} className="relative min-w-0 flex-1">
          {loading && !snapshot && <div className="flex h-full items-center justify-center gap-2 text-muted"><Spinner />Loading schema…</div>}
          {error && <ErrorBox error={error} />}
          {snapshot && !model.tables.length && <Empty icon={<Workflow className="size-8" />} title="No tables in this schema" />}
          {snapshot && model.tables.length > 0 && (
            <ReactFlow
              nodes={nodes}
              edges={edges}
              nodeTypes={nodeTypes}
              onNodesChange={onNodesChange}
              onNodeClick={onNodeClick}
              onNodeDoubleClick={onNodeDoubleClick}
              onNodeDragStop={onNodeDragStop}
              onPaneClick={() => setFocus(null)}
              onEdgeMouseEnter={(_, e) => setHoverEdge(e.id)}
              onEdgeMouseLeave={() => setHoverEdge(null)}
              nodesConnectable={false}
              edgesFocusable={false}
              onlyRenderVisibleElements={nodes.length > 60}
              minZoom={0.05}
              maxZoom={2.5}
              colorMode="dark"
              proOptions={{ hideAttribution: true }}
              style={{ background: "var(--color-app)" }}
            >
              <Background variant={BackgroundVariant.Dots} gap={18} size={1} color="var(--color-line)" />
              <MiniMap
                pannable
                zoomable
                style={{ background: "var(--color-panel)", border: "1px solid var(--color-line)" }}
                maskColor="color-mix(in srgb, var(--color-app) 70%, transparent)"
                nodeColor={(n) => ((n.data as TableNodeData).state === "dim" ? "var(--color-line)" : (n.data as TableNodeData).state === "normal" ? "var(--color-faint)" : "var(--color-accent)")}
                nodeStrokeWidth={0}
              />
              <Controls showInteractive={false} />
            </ReactFlow>
          )}
          {focusInfo && (
            <div className="absolute left-3 top-3 z-10 w-64 rounded-md border border-line bg-panel/95 p-2.5 text-xs shadow-xl backdrop-blur">
              <div className="mb-1 flex items-center justify-between">
                <span className="truncate font-semibold">{model.label(focusInfo)}</span>
                <IconButton title="Clear selection" onClick={() => setFocus(null)}><X className="size-3.5" /></IconButton>
              </div>
              <div className="space-y-0.5 text-muted">
                <div>{focusInfo.columns.length} columns · {focusInfo.indexes.length} indexes</div>
                <div>References: {model.rels.filter((r) => r.source === focus).map((r) => model.label(model.byKey.get(r.target)!)).join(", ") || "—"}</div>
                <div>Referenced by: {model.rels.filter((r) => r.target === focus).map((r) => model.label(model.byKey.get(r.source)!)).join(", ") || "—"}</div>
                {focusInfo.comment && <div className="italic">{focusInfo.comment}</div>}
              </div>
              <div className="mt-1.5 text-[10.5px] text-faint">Double-click a table to open its designer.</div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
