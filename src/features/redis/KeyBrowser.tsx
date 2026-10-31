// Left pane of the Redis tab: db selector, prefix/glob search, virtualized key tree/list.
import { useVirtualizer } from "@tanstack/react-virtual";
import clsx from "clsx";
import { ChevronDown, ChevronRight, Folder, FolderOpen, KeyRound, Plus, RefreshCw, Search, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Button, IconButton, Input, Select, Spinner, Spin } from "../../components/ui";
import { api, errMsg } from "../../lib/api";
import type { TreeNode } from "../../lib/types";
import { REDIS_TYPES, cmd, parseScan, replyError, typeMeta } from "./util";

interface Row {
  id: string;
  depth: number;
  kind: "group" | "key" | "info" | "more";
  label: string;
  key?: string;
  type?: string | null;
  count?: string | null;
  node?: TreeNode;
}

const isGlob = (p: string) => /[*?[\]]/.test(p);

export function KeyBrowser({ connId, db, onDb, selected, onSelect, onNewKey, reloadToken }: {
  connId: string;
  db: string;
  onDb: (db: string) => void;
  selected: string | null;
  onSelect: (key: string) => void;
  onNewKey: () => void;
  /** bump to reload (after create/delete/rename) */
  reloadToken: number;
}) {
  const [dbs, setDbs] = useState<TreeNode[]>([]);
  const [pattern, setPattern] = useState("");
  const [query, setQuery] = useState("");
  const [typeFilter, setTypeFilter] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // tree mode state
  const [roots, setRoots] = useState<TreeNode[]>([]);
  const [kids, setKids] = useState<Record<string, TreeNode[]>>({});
  const [open, setOpen] = useState<Set<string>>(new Set());
  const [loadingGroups, setLoadingGroups] = useState<Set<string>>(new Set());

  // scan mode state
  const [scan, setScan] = useState<{ keys: { key: string; type?: string }[]; cursor: string; scanned: number } | null>(null);
  const scanMode = isGlob(query) || !!typeFilter;

  const loadDbs = useCallback(() => {
    api.children(connId, []).then(setDbs).catch(() => setDbs([]));
  }, [connId]);
  useEffect(() => { loadDbs(); }, [loadDbs, reloadToken]);

  const rootPath = useMemo(() => (query && !scanMode ? [`db${db}`, query] : [`db${db}`]), [db, query, scanMode]);

  const loadTree = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const r = await api.children(connId, rootPath);
      setRoots(r);
      // re-fetch expanded groups so the view stays consistent after edits
      const entries = await Promise.all([...open].map(async (k) => {
        try { return [k, await api.children(connId, JSON.parse(k))] as const; } catch { return null; }
      }));
      setKids(Object.fromEntries(entries.filter(Boolean) as [string, TreeNode[]][]));
    } catch (e) {
      setError(errMsg(e));
      setRoots([]);
    } finally {
      setLoading(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connId, rootPath]);

  const runScan = useCallback(async (cursor: string, prev: { key: string; type?: string }[]) => {
    setLoading(true);
    setError(null);
    try {
      const pat = query ? (isGlob(query) ? query : `${query}*`) : "*";
      const keys: string[] = [];
      let cur = cursor;
      let scanned = 0;
      // A few round trips per click so sparse matches still return something.
      for (let i = 0; i < 10; i++) {
        const args: (string | number)[] = ["SCAN", cur, "MATCH", pat, "COUNT", 1000];
        if (typeFilter) args.push("TYPE", typeFilter);
        const [r] = await api.execute(connId, cmd(...args), { database: db, skipHistory: true });
        const p = parseScan(r);
        keys.push(...p.keys);
        cur = p.cursor;
        scanned++;
        if (cur === "0" || keys.length >= 200) break;
      }
      let typed = keys.map((key) => ({ key, type: typeFilter || undefined }));
      if (!typeFilter && keys.length) {
        const rs = await api.execute(connId, keys.map((k) => cmd("TYPE", k)).join("\n"), { database: db, maxRows: 1, skipHistory: true });
        if (!replyError(rs)) typed = keys.map((key, i) => ({ key, type: rs[i]?.message ?? undefined }));
      }
      const seen = new Set(prev.map((k) => k.key));
      setScan({ keys: [...prev, ...typed.filter((k) => !seen.has(k.key))], cursor: cur, scanned });
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setLoading(false);
    }
  }, [connId, db, query, typeFilter]);

  useEffect(() => {
    if (scanMode) { setScan(null); runScan("0", []); }
    else { setScan(null); loadTree(); }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scanMode, query, typeFilter, db, connId, reloadToken]);

  // Clear expansion state when switching db / prefix
  useEffect(() => { setOpen(new Set()); setKids({}); }, [db, query]);

  const toggle = async (n: TreeNode) => {
    const k = JSON.stringify(n.path);
    const next = new Set(open);
    if (next.has(k)) {
      next.delete(k);
      setOpen(next);
      return;
    }
    next.add(k);
    setOpen(next);
    if (!kids[k]) {
      setLoadingGroups((s) => new Set(s).add(k));
      try {
        const c = await api.children(connId, n.path);
        setKids((m) => ({ ...m, [k]: c }));
      } catch (e) {
        setError(errMsg(e));
      } finally {
        setLoadingGroups((s) => { const x = new Set(s); x.delete(k); return x; });
      }
    }
  };

  const rows: Row[] = useMemo(() => {
    if (scanMode) {
      const out: Row[] = (scan?.keys ?? []).map((k) => ({ id: `k:${k.key}`, depth: 0, kind: "key", label: k.key, key: k.key, type: k.type }));
      if (scan && scan.cursor !== "0") out.push({ id: "more", depth: 0, kind: "more", label: "Load more" });
      return out;
    }
    const out: Row[] = [];
    const prefixRoot = query && !scanMode ? query : "";
    const walk = (nodes: TreeNode[], depth: number) => {
      for (const n of nodes) {
        const pk = JSON.stringify(n.path);
        const label = depth === 0 && prefixRoot ? prefixRoot + n.label : n.label;
        if (n.kind === "keyGroup") {
          out.push({ id: pk, depth, kind: "group", label, count: n.detail, node: n });
          if (open.has(pk)) {
            if (loadingGroups.has(pk)) out.push({ id: pk + ":loading", depth: depth + 1, kind: "info", label: "Loading…" });
            else walk(kids[pk] ?? [], depth + 1);
          }
        } else if (n.kind === "key") {
          out.push({ id: pk, depth, kind: "key", label, key: n.table?.name ?? label, type: n.detail, node: n });
        } else {
          out.push({ id: pk, depth, kind: "info", label: n.label + (n.detail ? ` — ${n.detail}` : "") });
        }
      }
    };
    walk(roots, 0);
    return out;
  }, [scanMode, scan, roots, kids, open, loadingGroups, query]);

  const parentRef = useRef<HTMLDivElement>(null);
  const virt = useVirtualizer({ count: rows.length, getScrollElement: () => parentRef.current, estimateSize: () => 24, overscan: 20 });

  // Keyboard navigation over key rows
  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    e.preventDefault();
    const keyRows = rows.map((r, i) => [r, i] as const).filter(([r]) => r.kind === "key");
    const cur = keyRows.findIndex(([r]) => r.key === selected);
    const nxt = keyRows[Math.max(0, Math.min(keyRows.length - 1, cur + (e.key === "ArrowDown" ? 1 : -1)))];
    if (nxt) {
      onSelect(nxt[0].key!);
      virt.scrollToIndex(nxt[1], { align: "auto" });
    }
  };

  const dbOptions = dbs.length ? dbs : Array.from({ length: 16 }, (_, i) => ({ label: `db${i}`, detail: null } as TreeNode));
  const total = scanMode ? scan?.keys.length ?? 0 : rows.filter((r) => r.kind === "key").length;

  return (
    <div className="flex h-full flex-col bg-panel">
      <div className="flex items-center gap-1 border-b border-line p-1.5">
        <Select value={db} onChange={(e) => onDb(e.target.value.replace(/^db/, ""))} className="h-6 w-auto min-w-0 flex-1 text-xs" title="Database">
          {dbOptions.map((d) => (
            <option key={d.label} value={d.label.replace(/^db/, "")}>{d.label}{d.detail ? ` (${d.detail})` : ""}</option>
          ))}
        </Select>
        <IconButton title="New key" onClick={onNewKey}><Plus className="size-3.5" /></IconButton>
        <IconButton title="Refresh" onClick={() => { loadDbs(); if (scanMode) runScan("0", []); else loadTree(); }}>
          <Spin icon={RefreshCw} spinning={loading} className="size-3.5" />
        </IconButton>
      </div>
      <div className="flex items-center gap-1 border-b border-line p-1.5">
        <div className="relative flex-1">
          <Search className="pointer-events-none absolute left-2 top-1.5 size-3.5 text-faint" />
          <Input
            value={pattern}
            onChange={(e) => setPattern(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") setQuery(pattern.trim());
              if (e.key === "Escape") { setPattern(""); setQuery(""); }
            }}
            placeholder="Prefix or glob (user:*), Enter"
            className="h-6 pl-7 pr-6 font-mono text-xs"
          />
          {pattern && (
            <button className="absolute right-1.5 top-1.5 text-faint hover:text-fg" onClick={() => { setPattern(""); setQuery(""); }} title="Clear">
              <X className="size-3.5" />
            </button>
          )}
        </div>
        <Select value={typeFilter} onChange={(e) => setTypeFilter(e.target.value)} className="h-6 w-20 text-xs" title="Filter by type (SCAN … TYPE, Redis 6+)">
          <option value="">All</option>
          {REDIS_TYPES.filter((t) => t !== "ReJSON-RL").map((t) => <option key={t} value={t}>{t}</option>)}
        </Select>
      </div>
      <div
        ref={parentRef}
        tabIndex={0}
        onKeyDown={onKeyDown}
        className="min-h-0 flex-1 overflow-auto outline-none"
      >
        {error && <div className="selectable p-3 text-xs text-danger">{error}</div>}
        {!error && !loading && !rows.length && (
          <div className="p-6 text-center text-xs text-faint">{query || typeFilter ? "No matching keys" : "This database is empty"}</div>
        )}
        {loading && !rows.length && <div className="flex items-center gap-2 p-3 text-xs text-muted"><Spinner className="size-3.5" />Loading keys…</div>}
        <div style={{ height: virt.getTotalSize(), position: "relative" }}>
          {virt.getVirtualItems().map((vi) => {
            const r = rows[vi.index];
            return (
              <div key={r.id} style={{ position: "absolute", top: 0, left: 0, right: 0, height: vi.size, transform: `translateY(${vi.start}px)` }}>
                <RowView row={r} selected={r.kind === "key" && r.key === selected} open={open.has(r.id)}
                  onClick={() => {
                    if (r.kind === "group" && r.node) toggle(r.node);
                    else if (r.kind === "key" && r.key) onSelect(r.key);
                    else if (r.kind === "more" && scan) runScan(scan.cursor, scan.keys);
                  }}
                  loading={loading && r.kind === "more"}
                />
              </div>
            );
          })}
        </div>
      </div>
      <div className="flex h-6 shrink-0 items-center gap-2 border-t border-line px-2 text-[11px] text-faint">
        <span>{total.toLocaleString()} key{total === 1 ? "" : "s"}{scanMode && scan && scan.cursor !== "0" ? "+" : ""}</span>
        <span className="truncate">{scanMode ? `SCAN ${query || "*"}${typeFilter ? ` TYPE ${typeFilter}` : ""}` : query ? `prefix "${query}"` : "grouped by \":\""}</span>
        <div className="flex-1" />
        <Button size="sm" variant="ghost" className="h-5 px-1" onClick={onNewKey}><Plus className="size-3" />Key</Button>
      </div>
    </div>
  );
}

function RowView({ row, selected, open, onClick, loading }: { row: Row; selected: boolean; open: boolean; onClick: () => void; loading?: boolean }) {
  const pad = 6 + row.depth * 14;
  if (row.kind === "more") {
    return (
      <button onClick={onClick} className="flex h-6 w-full items-center justify-center gap-1.5 text-xs text-accent hover:bg-hover">
        {loading ? <Spinner className="size-3" /> : null}Load more…
      </button>
    );
  }
  if (row.kind === "info") {
    return <div className="flex h-6 items-center truncate text-[11px] italic text-faint" style={{ paddingLeft: pad + 16 }}>{row.label}</div>;
  }
  const tm = typeMeta(row.type);
  return (
    <div
      onClick={onClick}
      title={row.key ?? row.label}
      className={clsx("flex h-6 cursor-pointer items-center gap-1.5 pr-2 text-[12.5px]", selected ? "bg-accent/15 text-fg" : "hover:bg-hover")}
      style={{ paddingLeft: pad }}
    >
      {row.kind === "group" ? (
        <>
          {open ? <ChevronDown className="size-3 shrink-0 text-faint" /> : <ChevronRight className="size-3 shrink-0 text-faint" />}
          {open ? <FolderOpen className="size-3.5 shrink-0 text-warn/80" /> : <Folder className="size-3.5 shrink-0 text-warn/80" />}
          <span className="truncate">{row.label}</span>
          <span className="ml-auto shrink-0 text-[10.5px] text-faint">{row.count}</span>
        </>
      ) : (
        <>
          <span className="w-3 shrink-0" />
          {row.type ? (
            <span className={clsx("w-11 shrink-0 rounded px-1 text-center font-mono text-[9.5px] font-semibold leading-4", tm.cls)}>{tm.label}</span>
          ) : <KeyRound className="size-3.5 shrink-0 text-faint" />}
          <span className="truncate font-mono text-[12px]">{row.label}</span>
        </>
      )}
    </div>
  );
}
