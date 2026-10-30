// Lens-style Kubernetes cluster view.
import clsx from "clsx";
import {
  AlertTriangle, AppWindow, Bot, Boxes, Package, ChevronDown, ChevronRight, CloudOff, Gauge, Layers, ListTree, Pencil, Puzzle, RefreshCw, Search,
  Server, Settings2, Network, HardDrive, Shield, X,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
import { Button, ErrorBox, IconButton, Input, Spinner } from "../../components/ui";
import { errMsg } from "../../lib/api";
import { useConnections } from "../../store/connections";
import { openConnectionAgent } from "../agent/connection";
import { useUi } from "../../store/ui";
import type { Tab } from "../../tabs/types";
import { eventsReady, k8s, type ConnectInfo, type K8sObject } from "./api";
import DetailsDrawer, { type DrawerActions } from "./DetailsDrawer";
import Dock, { type DockSession } from "./Dock";
import { useAsync, useMetrics, useWatchList } from "./hooks";
import NamespacePicker from "./NamespacePicker";
import Overview from "./Overview";
import PortForwards from "./PortForwards";
import ResourceTable from "./ResourceTable";
import Applications from "./Applications";
import HelmView from "./HelmView";
import WorkloadsOverview from "./WorkloadsOverview";
import { openForWorkload, useRowActions, type DetailsTab, type RowActionHost } from "./rowActions";
import { KINDS, NAV, crdKindDef, crdKindKey, genericKindDef, type KindDef, type View } from "./resources";

export default function K8sTab({ tab }: { tab: Extract<Tab, { type: "k8s" }> }) {
  const connId = tab.connId;
  const markOpen = useConnections((s) => s.markOpenLocal);
  const openDialog = useUi((s) => s.openConnectionDialog);
  const [info, setInfo] = useState<ConnectInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [connecting, setConnecting] = useState(true);

  const connect = useCallback(async () => {
    setConnecting(true);
    setError(null);
    try {
      await eventsReady();
      const i = await k8s.connect(connId);
      setInfo(i);
      markOpen(connId, true);
    } catch (e) {
      setInfo(null);
      setError(errMsg(e));
    } finally {
      setConnecting(false);
    }
  }, [connId, markOpen]);

  useEffect(() => {
    connect();
    return () => {
      k8s.disconnect(connId).catch(() => {});
      markOpen(connId, false);
    };
  }, [connId, connect, markOpen]);

  if (!info) {
    return (
      <div className="flex h-full items-center justify-center p-6">
        {connecting ? (
          <div className="flex flex-col items-center gap-3 text-muted"><Spinner className="size-6" /><span className="text-xs">Connecting to cluster…</span></div>
        ) : (
          <div className="flex w-full max-w-xl flex-col items-center gap-3 text-center">
            <CloudOff className="size-10 text-faint" />
            <div className="font-medium">Could not connect to the cluster</div>
            <div className="w-full text-left"><ErrorBox error={error ?? "Unknown error"} /></div>
            <div className="flex gap-2">
              <Button variant="primary" onClick={connect}><RefreshCw className="size-3.5" />Retry</Button>
              <Button onClick={() => openDialog(connId)}><Pencil className="size-3.5" />Edit connection</Button>
            </div>
          </div>
        )}
      </div>
    );
  }
  return <ClusterView connId={connId} info={info} reconnect={connect} />;
}

// ---------------------------------------------------------------------------

interface Selection { kind: string; namespace?: string; name: string; tab?: { tab: DetailsTab; nonce: number } }

function useStored<T>(key: string, initial: T): [T, (v: T) => void] {
  const [v, setV] = useState<T>(() => {
    try { const raw = localStorage.getItem(key); return raw ? (JSON.parse(raw) as T) : initial; } catch { return initial; }
  });
  const set = useCallback((nv: T) => { setV(nv); try { localStorage.setItem(key, JSON.stringify(nv)); } catch { /* quota */ } }, [key]);
  return [v, set];
}

const GROUP_ICONS: Record<string, ReactNode> = {
  workloads: <Boxes className="size-4" />,
  config: <Settings2 className="size-4" />,
  network: <Network className="size-4" />,
  storage: <HardDrive className="size-4" />,
  access: <Shield className="size-4" />,
};

function ClusterView({ connId, info, reconnect }: { connId: string; info: ConnectInfo; reconnect: () => void }) {
  const readOnly = useConnections((s) => s.connections.find((c) => c.id === connId)?.readOnly ?? false);
  const [view, setViewRaw] = useStored<View>(`devcat.k8s.view.${connId}`, { type: "overview" });
  const [selectedNs, setSelectedNs] = useStored<string[]>(`devcat.k8s.ns.${connId}`, []);
  const [search, setSearch] = useState("");
  const [selection, setSelection] = useState<Selection | null>(null);
  const [drawerW, setDrawerW] = useStored<number>("devcat.k8s.drawerW", 560);
  const [dockH, setDockH] = useStored<number>("devcat.k8s.dockH", 300);
  const [dockCollapsed, setDockCollapsed] = useState(false);
  const [sessions, setSessions] = useState<DockSession[]>([]);
  const [activeSession, setActiveSession] = useState<string | null>(null);

  const setView = useCallback((v: View) => { setViewRaw(v); setSearch(""); }, [setViewRaw]);

  const namespaces = useAsync(() => k8s.namespaces(connId), [connId], 60000);
  const crds = useAsync(() => k8s.list(connId, "customresourcedefinitions").catch(() => [] as K8sObject[]), [connId], 120000);

  const kind = view.type === "list" ? view.kind : null;
  const def: KindDef | null = useMemo(() => {
    if (!kind) return null;
    if (KINDS[kind]) return KINDS[kind];
    const crd = (crds.data ?? []).find((c) => crdKindKey(c) === kind);
    return crd ? crdKindDef(crd) : genericKindDef(kind);
  }, [kind, crds.data]);

  // Wait for CRD scope before watching custom resources so namespace filtering is right.
  const watchKind = kind && (KINDS[kind] || crds.data) ? kind : null;
  const list = useWatchList(connId, watchKind, def?.namespaced ?? false, selectedNs.length ? selectedNs : null);
  const metricsNs = selectedNs.length === 1 ? selectedNs[0] : null;
  const { metrics, available: metricsAvailable } = useMetrics(connId, kind === "pods" ? "pods" : kind === "nodes" ? "nodes" : null, metricsNs);

  // Resolve the selected object: live from the current list, otherwise fetched once.
  const liveObj = useMemo(() => {
    if (!selection || selection.kind !== kind) return null;
    return list.items.find((o) => o.metadata.name === selection.name && (o.metadata.namespace || undefined) === (selection.namespace || undefined)) ?? null;
  }, [selection, kind, list.items]);
  const needFetch = !!selection && !liveObj && (selection.kind !== kind || !list.loading);
  const fetched = useAsync(
    () => (needFetch && selection ? k8s.get(connId, selection.kind, selection.namespace, selection.name) : Promise.resolve(null)),
    [connId, needFetch, selection?.kind, selection?.namespace, selection?.name],
  );
  const drawerObj = liveObj ?? (needFetch ? fetched.data : null);

  const openSession = useCallback((type: "logs" | "shell", pod: K8sObject, container?: string, extra?: { owner?: string; pods?: string[] }) => {
    const containers: string[] = (pod.spec?.containers ?? []).map((c: K8sObject) => c.name);
    const initContainers: string[] = (pod.spec?.initContainers ?? []).map((c: K8sObject) => c.name);
    const ns = pod.metadata.namespace!;
    setSessions((prev) => {
      const existing = type === "logs" && prev.find((s) => s.type === "logs" && s.namespace === ns && (extra?.owner ? s.owner === extra.owner : s.pod === pod.metadata.name && !s.owner) && (!container || s.container === container));
      if (existing) { setActiveSession(existing.id); return prev; }
      const id = `${type}-${ns}-${pod.metadata.name}-${Date.now()}`;
      setActiveSession(id);
      const s: DockSession = type === "logs"
        ? { id, type, namespace: ns, pod: pod.metadata.name, containers, initContainers, container, ...extra }
        : { id, type, namespace: ns, pod: pod.metadata.name, containers, container, ...extra };
      return [...prev, s];
    });
    setDockCollapsed(false);
  }, []);

  const closeSession = useCallback((id: string) => {
    setSessions((prev) => {
      const idx = prev.findIndex((s) => s.id === id);
      const next = prev.filter((s) => s.id !== id);
      setActiveSession((a) => (a === id ? next[Math.min(idx, next.length - 1)]?.id ?? null : a));
      return next;
    });
  }, []);

  const updateSession = useCallback((id: string, patch: { pod: string }) => {
    setSessions((prev) => prev.map((s) => (s.id === id ? { ...s, ...patch } : s)));
  }, []);

  const navigate = useCallback((kindKey: string, namespace?: string, name?: string) => {
    setView({ type: "list", kind: kindKey });
    setSelection(name ? { kind: kindKey, namespace: namespace || undefined, name } : null);
  }, [setView]);

  const host: RowActionHost = useMemo(() => ({
    connId,
    readOnly,
    openSession: (type, pod, opts) => openSession(type, pod, undefined, opts),
    showDetails: (kindKey, obj, t) => setSelection({
      kind: kindKey, namespace: obj.metadata.namespace || undefined, name: obj.metadata.name,
      tab: t ? { tab: t, nonce: Date.now() } : undefined,
    }),
    onDeleted: (obj) => setSelection((s) => (s && s.name === obj.metadata.name && (s.namespace ?? "") === (obj.metadata.namespace ?? "") ? null : s)),
    browseCrd: (crd) => navigate(crdKindKey(crd)),
  }), [connId, readOnly, openSession, navigate]);
  const rowActions = useRowActions(host);

  const actions: DrawerActions = useMemo(() => ({
    logs: (pod, c) => openSession("logs", pod, c),
    shell: (pod, c) => openSession("shell", pod, c),
    workload: (type, obj) => void openForWorkload(host, type, obj),
    navigate: (k, ns, name) => navigate(k, KINDS[k] && !KINDS[k].namespaced ? undefined : ns, name),
    browseCrd: (crd) => navigate(crdKindKey(crd)),
  }), [openSession, navigate, host]);

  const title = view.type === "overview" ? "Overview" : view.type === "portforwards" ? "Port Forwarding"
    : view.type === "workloads" ? "Workloads" : view.type === "applications" ? "Applications" : view.type === "helm" ? "Helm Releases" : def?.title ?? "";
  // Views that get the search + namespace header.
  const extraView = view.type === "workloads" || view.type === "applications" || view.type === "helm";

  return (
    <div className="flex h-full min-h-0 flex-col bg-app">
      <div className="flex min-h-0 flex-1">
        <Sidebar connId={connId} info={info} view={view} setView={(v) => { setView(v); setSelection(null); }} crds={crds.data ?? []} reconnect={reconnect} />
        <div className="flex min-w-0 flex-1 flex-col bg-panel">
          <div className="flex min-h-0 flex-1">
            <div className="flex min-w-0 flex-1 flex-col">
              {((view.type === "list" && def) || extraView) && (
                <div className="flex h-10 shrink-0 items-center gap-2 border-b border-line px-3">
                  <span className="text-[14px] font-semibold">{title}</span>
                  {!extraView && !list.loading && !list.error && <span className="text-xs text-muted tabular-nums">{list.items.length}</span>}
                  {list.warning && <span className="flex items-center gap-1 text-xs text-warn" title={list.warning}><AlertTriangle className="size-3.5" />reconnecting…</span>}
                  {(kind === "pods" || kind === "nodes") && !metricsAvailable && <span className="text-[11px] text-faint" title="metrics.k8s.io not available">no metrics</span>}
                  <div className="flex-1" />
                  {view.type !== "workloads" && <div className="relative w-64">
                    <Search className="pointer-events-none absolute left-2 top-1/2 size-3.5 -translate-y-1/2 text-faint" />
                    <Input className="pl-7 pr-6" placeholder={`Search ${title.toLowerCase()}…`} value={search} onChange={(e) => setSearch(e.target.value)}
                      onKeyDown={(e) => e.key === "Escape" && setSearch("")} />
                    {search && <button className="absolute right-1.5 top-1/2 -translate-y-1/2 text-muted hover:text-fg" onClick={() => setSearch("")}><X className="size-3.5" /></button>}
                  </div>}
                  <NamespacePicker namespaces={namespaces.data ?? []} selected={selectedNs} onChange={setSelectedNs} disabled={!extraView && !def?.namespaced} />
                </div>
              )}
              <div className="min-h-0 flex-1">
                {view.type === "overview" ? (
                  <Overview connId={connId} onNavigate={(k, ns, name) => navigate(k, ns, name)} />
                ) : view.type === "workloads" ? (
                  <WorkloadsOverview connId={connId} namespaces={selectedNs} onNavigate={(k) => navigate(k)} />
                ) : view.type === "applications" ? (
                  <Applications connId={connId} namespaces={selectedNs} search={search} menuFor={rowActions.menuFor} onSelect={(k, o) => host.showDetails(k, o)} />
                ) : view.type === "helm" ? (
                  <HelmView connId={connId} namespaces={selectedNs} search={search} />
                ) : view.type === "portforwards" ? (
                  <div className="flex h-full flex-col">
                    <div className="flex h-10 shrink-0 items-center border-b border-line px-3 text-[14px] font-semibold">Port Forwarding</div>
                    <div className="min-h-0 flex-1"><PortForwards connId={connId} /></div>
                  </div>
                ) : def && list.error ? (
                  <div className="p-2">
                    <ErrorBox error={list.error} />
                    <div className="flex gap-2 px-3">
                      <Button onClick={list.retry}><RefreshCw className="size-3.5" />Retry</Button>
                      <Button variant="ghost" onClick={reconnect}>Reconnect</Button>
                    </div>
                  </div>
                ) : def && (list.loading || !watchKind) ? (
                  <div className="flex h-full items-center justify-center"><Spinner /></div>
                ) : def ? (
                  <ResourceTable
                    key={def.key}
                    def={def}
                    items={list.items}
                    ctx={{ metrics }}
                    search={search}
                    selectedUid={drawerObj?.metadata.uid}
                    hideNamespace={!def.namespaced || selectedNs.length === 1}
                    onSelect={(o) => setSelection({ kind: def.key, namespace: o.metadata.namespace, name: o.metadata.name })}
                    onActivate={(o) => {
                      if (def.key === "customresourcedefinitions") navigate(crdKindKey(o));
                      else if (def.key === "pods") openSession("logs", o);
                    }}
                    menuFor={(o) => rowActions.menuFor(def.key, o)}
                  />
                ) : null}
              </div>
            </div>
            {selection && (
              <DetailsDrawer
                connId={connId}
                kindKey={selection.kind}
                obj={drawerObj ?? null}
                loading={(selection.kind === kind && list.loading) || fetched.loading}
                readOnly={readOnly}
                onClose={() => setSelection(null)}
                actions={actions}
                width={drawerW}
                setWidth={setDrawerW}
                initialTab={selection.tab}
              />
            )}
          </div>
          {rowActions.node}
          <Dock
            connId={connId}
            sessions={sessions}
            activeId={activeSession}
            setActive={setActiveSession}
            close={closeSession}
            update={updateSession}
            height={dockH}
            setHeight={setDockH}
            collapsed={dockCollapsed}
            setCollapsed={setDockCollapsed}
          />
        </div>
      </div>
    </div>
  );
}

// ---- left navigation --------------------------------------------------------------

function NavItem({ label, active, onClick, icon, indent }: { label: string; active: boolean; onClick: () => void; icon?: ReactNode; indent?: boolean }) {
  return (
    <button
      onClick={onClick}
      className={clsx(
        "flex h-7 w-full items-center gap-2 rounded px-2 text-left text-[13px]",
        indent && "pl-8",
        active ? "bg-accent/15 text-accent" : "text-fg/85 hover:bg-hover",
      )}
    >
      {icon && <span className={active ? "text-accent" : "text-muted"}>{icon}</span>}
      <span className="truncate">{label}</span>
    </button>
  );
}

function Group({ id, title, icon, children, active }: { id: string; title: string; icon: ReactNode; children: ReactNode; active: boolean }) {
  const [open, setOpen] = useStored<boolean>(`devcat.k8s.nav.${id}`, true);
  return (
    <div>
      <button onClick={() => setOpen(!open)} className={clsx("flex h-7 w-full items-center gap-2 rounded px-2 text-[13px] hover:bg-hover", active && !open ? "text-accent" : "text-fg/85")}>
        <span className="text-muted">{icon}</span>
        <span className="flex-1 truncate text-left">{title}</span>
        {open ? <ChevronDown className="size-3.5 text-faint" /> : <ChevronRight className="size-3.5 text-faint" />}
      </button>
      {open && <div className="flex flex-col">{children}</div>}
    </div>
  );
}

function isActive(view: View, v: View) {
  if (view.type !== v.type) return false;
  return view.type !== "list" || (v.type === "list" && view.kind === v.kind);
}

function Sidebar({ connId, info, view, setView, crds, reconnect }: { connId: string; info: ConnectInfo; view: View; setView: (v: View) => void; crds: K8sObject[]; reconnect: () => void }) {
  const conn = useConnections((s) => s.connections.find((c) => c.id === connId));
  const [crdFilter, setCrdFilter] = useState("");
  const crdGroups = useMemo(() => {
    const m = new Map<string, K8sObject[]>();
    for (const c of crds) {
      const g = c.spec?.group ?? "";
      if (crdFilter && !`${g} ${c.spec?.names?.kind}`.toLowerCase().includes(crdFilter.toLowerCase())) continue;
      m.set(g, [...(m.get(g) ?? []), c]);
    }
    return [...m.entries()].sort(([a], [b]) => a.localeCompare(b)).map(([g, list]) => [g, list.sort((a, b) => a.spec.names.kind.localeCompare(b.spec.names.kind))] as const);
  }, [crds, crdFilter]);
  const crdActive = view.type === "list" && view.kind.includes("/");

  return (
    <div className="flex w-56 shrink-0 flex-col border-r border-line bg-app">
      <div className="flex h-10 shrink-0 items-center gap-2 border-b border-line px-3">
        <span className="size-2 shrink-0 rounded-full bg-ok" title="Connected" />
        <div className="min-w-0 flex-1">
          <div className="truncate text-[12.5px] font-semibold" title={info.context}>{info.context}</div>
          <div className="truncate text-[10.5px] text-muted" title={info.server}>{info.version}</div>
        </div>
        {conn && <IconButton title="Ask DevCat Agent about this cluster" onClick={() => void openConnectionAgent(conn)}><Bot className="size-3.5" /></IconButton>}
        <IconButton title="Reconnect" onClick={reconnect}><RefreshCw className="size-3.5" /></IconButton>
      </div>
      <nav className="min-h-0 flex-1 overflow-y-auto p-1.5">
        <NavItem label="Overview" icon={<Gauge className="size-4" />} active={view.type === "overview"} onClick={() => setView({ type: "overview" })} />
        <NavItem label="Applications" icon={<AppWindow className="size-4" />} active={view.type === "applications"} onClick={() => setView({ type: "applications" })} />
        <NavItem label="Nodes" icon={<Server className="size-4" />} active={isActive(view, { type: "list", kind: "nodes" })} onClick={() => setView({ type: "list", kind: "nodes" })} />
        {NAV.map((g) => (
          <Group key={g.id} id={g.id} title={g.title} icon={GROUP_ICONS[g.id]} active={g.items.some((i) => isActive(view, i.view))}>
            {g.items.map((i) => <NavItem key={i.label} label={i.label} indent active={isActive(view, i.view)} onClick={() => setView(i.view)} />)}
          </Group>
        ))}
        <NavItem label="Namespaces" icon={<Layers className="size-4" />} active={isActive(view, { type: "list", kind: "namespaces" })} onClick={() => setView({ type: "list", kind: "namespaces" })} />
        <NavItem label="Events" icon={<ListTree className="size-4" />} active={isActive(view, { type: "list", kind: "events" })} onClick={() => setView({ type: "list", kind: "events" })} />
        <Group id="helm" title="Helm" icon={<Package className="size-4" />} active={view.type === "helm"}>
          <NavItem label="Releases" indent active={view.type === "helm"} onClick={() => setView({ type: "helm" })} />
        </Group>
        <Group id="crd" title="Custom Resources" icon={<Puzzle className="size-4" />} active={crdActive || isActive(view, { type: "list", kind: "customresourcedefinitions" })}>
          <NavItem label="Definitions" indent active={isActive(view, { type: "list", kind: "customresourcedefinitions" })} onClick={() => setView({ type: "list", kind: "customresourcedefinitions" })} />
          {crds.length > 8 && (
            <div className="py-1 pl-6 pr-1"><Input className="h-6 text-xs" placeholder="Filter CRDs…" value={crdFilter} onChange={(e) => setCrdFilter(e.target.value)} /></div>
          )}
          {crdGroups.map(([g, list]) => <CrdGroup key={g} group={g} crds={list} view={view} setView={setView} forceOpen={!!crdFilter} />)}
        </Group>
      </nav>
    </div>
  );
}

function CrdGroup({ group, crds, view, setView, forceOpen }: { group: string; crds: K8sObject[]; view: View; setView: (v: View) => void; forceOpen: boolean }) {
  const keys = crds.map(crdKindKey);
  const containsActive = view.type === "list" && keys.includes(view.kind);
  const [open, setOpen] = useState(containsActive);
  const shown = open || forceOpen;
  return (
    <div>
      <button onClick={() => setOpen(!open)} className={clsx("flex h-6 w-full items-center gap-1 rounded pl-6 pr-2 text-[12px] hover:bg-hover", containsActive ? "text-accent" : "text-muted")}>
        {shown ? <ChevronDown className="size-3 shrink-0" /> : <ChevronRight className="size-3 shrink-0" />}
        <span className="truncate" title={group}>{group}</span>
      </button>
      {shown && crds.map((c) => {
        const k = crdKindKey(c);
        return (
          <button
            key={c.metadata.uid}
            onClick={() => setView({ type: "list", kind: k })}
            className={clsx("flex h-6 w-full items-center rounded pl-11 pr-2 text-left text-[12.5px]", view.type === "list" && view.kind === k ? "bg-accent/15 text-accent" : "text-fg/80 hover:bg-hover")}
          >
            <span className="truncate">{c.spec.names.kind}</span>
          </button>
        );
      })}
    </div>
  );
}
