import clsx from "clsx";
import { AlertTriangle, Boxes, Cpu, Layers, MemoryStick, RefreshCw, Server } from "lucide-react";
import type { ReactNode } from "react";
import { Cell, Pie, PieChart, ResponsiveContainer, Tooltip } from "recharts";
import { Button, ErrorBox, IconButton, Spinner, Spin } from "../../components/ui";
import { k8s, type K8sObject, type ResourceStat } from "./api";
import { age, eventTime, fmtBytes, fmtCpu, nodeStatus, parseQty, useNow } from "./format";
import { useAsync, useMetrics } from "./hooks";
import { kindKeyFor } from "./resources";

// Status palette (reserved meanings; always paired with a text label).
const PHASE_COLORS: Record<string, string> = {
  Running: "var(--color-ok)",
  Pending: "var(--color-warn)",
  Failed: "var(--color-danger)",
  Succeeded: "var(--color-info)",
  Unknown: "var(--color-faint)",
};
const PHASE_ORDER = ["Running", "Pending", "Failed", "Succeeded", "Unknown"];

function Card({ title, icon, children, className, onClick }: { title: string; icon?: ReactNode; children: ReactNode; className?: string; onClick?: () => void }) {
  return (
    <div
      onClick={onClick}
      className={clsx("rounded-lg border border-line bg-raised/60 p-3", onClick && "cursor-pointer hover:border-accent/40", className)}
    >
      <div className="mb-2 flex items-center gap-1.5 text-[11px] font-semibold uppercase tracking-wider text-muted">{icon}{title}</div>
      {children}
    </div>
  );
}

function Stat({ value, total, label, tone }: { value: number; total?: number; label: string; tone?: "ok" | "warn" | "danger" }) {
  return (
    <div>
      <div className="flex items-baseline gap-1">
        <span className={clsx("text-2xl font-semibold tabular-nums", tone === "warn" && "text-warn", tone === "danger" && "text-danger")}>{value}</span>
        {total != null && <span className="text-sm text-muted tabular-nums">/ {total}</span>}
      </div>
      <div className="text-xs text-muted">{label}</div>
    </div>
  );
}

/** Horizontal meter: one row per measure against allocatable. */
function ResourceMeters({ stat, fmt }: { stat: ResourceStat; fmt: (n: number) => string }) {
  const max = stat.allocatable || stat.capacity || 1;
  const rows: { label: string; value: number | null | undefined; color: string }[] = [
    { label: "Usage", value: stat.usage, color: "bg-info" },
    { label: "Requests", value: stat.requests, color: "bg-accent" },
    { label: "Limits", value: stat.limits, color: "bg-muted" },
  ];
  return (
    <div className="flex flex-col gap-2">
      {rows.map((r) => {
        const pct = r.value != null ? (r.value / max) * 100 : null;
        return (
          <div key={r.label} className="grid grid-cols-[64px_1fr_120px] items-center gap-2 text-xs" title={pct != null ? `${pct.toFixed(1)}% of allocatable` : "Not available"}>
            <span className="text-muted">{r.label}</span>
            <div className="h-1.5 overflow-hidden rounded-full bg-hover">
              {pct != null && <div className={clsx("h-full rounded-full", pct > 100 ? "bg-danger" : r.color)} style={{ width: `${Math.min(100, pct)}%` }} />}
            </div>
            <span className="text-right tabular-nums">
              {r.value != null ? <>{fmt(r.value)} <span className="text-muted">({pct!.toFixed(0)}%)</span></> : <span className="text-faint">n/a</span>}
            </span>
          </div>
        );
      })}
      <div className="mt-0.5 text-[11px] text-faint">
        Allocatable {fmt(stat.allocatable)} · Capacity {fmt(stat.capacity)}
      </div>
    </div>
  );
}

function PhaseDonut({ phases, total }: { phases: Record<string, number>; total: number }) {
  const data = PHASE_ORDER.filter((p) => phases[p]).map((p) => ({ name: p, value: phases[p] }))
    .concat(Object.entries(phases).filter(([p]) => !PHASE_ORDER.includes(p)).map(([name, value]) => ({ name, value })));
  return (
    <div className="flex items-center gap-4">
      <div className="relative size-28 shrink-0">
        <ResponsiveContainer width="100%" height="100%">
          <PieChart>
            <Pie data={data} dataKey="value" nameKey="name" innerRadius={36} outerRadius={52} paddingAngle={data.length > 1 ? 2 : 0} stroke="var(--color-raised)" strokeWidth={2} isAnimationActive={false}>
              {data.map((d) => <Cell key={d.name} fill={PHASE_COLORS[d.name] ?? "var(--color-faint)"} />)}
            </Pie>
            <Tooltip
              contentStyle={{ background: "var(--color-panel)", border: "1px solid var(--color-line)", borderRadius: 6, fontSize: 12 }}
              itemStyle={{ color: "var(--color-fg)" }}
              formatter={(v, n) => [`${v} (${total ? Math.round((Number(v) / total) * 100) : 0}%)`, n]}
            />
          </PieChart>
        </ResponsiveContainer>
        <div className="pointer-events-none absolute inset-0 flex flex-col items-center justify-center">
          <span className="text-lg font-semibold tabular-nums">{total}</span>
          <span className="text-[10px] text-muted">pods</span>
        </div>
      </div>
      <div className="flex flex-col gap-1 text-xs">
        {data.map((d) => (
          <div key={d.name} className="flex items-center gap-2">
            <span className="size-2.5 rounded-sm" style={{ background: PHASE_COLORS[d.name] ?? "var(--color-faint)" }} />
            <span className="w-20">{d.name}</span>
            <span className="tabular-nums text-muted">{d.value}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

export default function Overview({ connId, onNavigate }: { connId: string; onNavigate: (kindKey: string, namespace?: string, name?: string) => void }) {
  const now = useNow();
  const { data, error, loading, reload } = useAsync(() => k8s.overview(connId), [connId], 30000);
  const nodes = useAsync(() => k8s.list(connId, "nodes"), [connId], 30000);
  const { metrics } = useMetrics(connId, "nodes", null);

  if (!data) {
    return error
      ? <div className="p-2"><ErrorBox error={error} /><div className="px-3"><Button onClick={reload}>Retry</Button></div></div>
      : <div className="flex h-full items-center justify-center"><Spinner /></div>;
  }
  const notReady = data.nodesTotal - data.nodesReady;
  const problemPods = (data.podsByPhase.Pending ?? 0) + (data.podsByPhase.Failed ?? 0) + (data.podsByPhase.Unknown ?? 0);

  return (
    <div className="h-full overflow-auto">
      <div className="mx-auto flex max-w-[1400px] flex-col gap-3 p-4">
        <div className="flex items-center gap-2">
          <div className="min-w-0 flex-1">
            <div className="text-[15px] font-semibold">Cluster overview</div>
            <div className="truncate text-xs text-muted">{data.context} · {data.server} · Kubernetes {data.version} ({data.platform})</div>
          </div>
          <IconButton title="Refresh" onClick={() => { reload(); nodes.reload(); }}><Spin icon={RefreshCw} spinning={loading} className="size-4" /></IconButton>
        </div>
        {data.errors.length > 0 && (
          <div className="rounded-md border border-warn/30 bg-warn/10 px-3 py-2 text-xs text-warn">
            Some data could not be loaded (insufficient permissions?): {data.errors.join("; ")}
          </div>
        )}

        <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
          <Card title="Nodes" icon={<Server className="size-3.5" />} onClick={() => onNavigate("nodes")}>
            <Stat value={data.nodesReady} total={data.nodesTotal} label={notReady ? `${notReady} not ready` : "ready"} tone={notReady ? "danger" : undefined} />
          </Card>
          <Card title="Pods" icon={<Boxes className="size-3.5" />} onClick={() => onNavigate("pods")}>
            <Stat value={data.podsByPhase.Running ?? 0} total={data.podsTotal} label={`running${problemPods ? ` · ${problemPods} need attention` : ""} · ${data.containerRestarts} restarts`} tone={problemPods ? "warn" : undefined} />
          </Card>
          <Card title="Deployments" icon={<Layers className="size-3.5" />} onClick={() => onNavigate("deployments")}>
            <Stat value={data.deploymentsReady} total={data.deploymentsTotal} label="fully available" tone={data.deploymentsReady < data.deploymentsTotal ? "warn" : undefined} />
          </Card>
          <Card title="Namespaces" icon={<Layers className="size-3.5" />} onClick={() => onNavigate("namespaces")}>
            <Stat value={data.namespaces} label={`pod slots ${data.podsTotal}/${data.podsCapacity}`} />
          </Card>
        </div>

        <div className="grid grid-cols-1 gap-3 lg:grid-cols-3">
          <Card title="Pod phases">
            {data.podsTotal ? <PhaseDonut phases={data.podsByPhase} total={data.podsTotal} /> : <div className="text-xs text-muted">No pods.</div>}
          </Card>
          <Card title="CPU" icon={<Cpu className="size-3.5" />}>
            <ResourceMeters stat={data.cpu} fmt={(n) => `${fmtCpu(n)} cores`.replace(/m cores$/, "m")} />
          </Card>
          <Card title="Memory" icon={<MemoryStick className="size-3.5" />}>
            <ResourceMeters stat={data.memory} fmt={fmtBytes} />
          </Card>
        </div>
        {!data.metricsAvailable && (
          <div className="text-[11px] text-faint">Live usage is unavailable — metrics-server (metrics.k8s.io) is not installed or not accessible.</div>
        )}

        <div className="grid grid-cols-1 gap-3 xl:grid-cols-2">
          <Card title={`Node usage${nodes.data ? ` (${nodes.data.length})` : ""}`} icon={<Server className="size-3.5" />}>
            <div className="flex max-h-80 flex-col gap-1 overflow-auto">
              {(nodes.data ?? []).sort((a, b) => a.metadata.name.localeCompare(b.metadata.name)).map((n) => (
                <NodeRow key={n.metadata.uid} node={n} cpu={metrics?.get(n.metadata.name)?.cpu} mem={metrics?.get(n.metadata.name)?.memory} onClick={() => onNavigate("nodes", undefined, n.metadata.name)} />
              ))}
              {nodes.error && <div className="text-xs text-danger">{nodes.error}</div>}
            </div>
          </Card>
          <Card title={`Warnings (${data.warnings.length})`} icon={<AlertTriangle className="size-3.5 text-warn" />}>
            <div className="flex max-h-80 flex-col overflow-auto">
              {data.warnings.length === 0 && <div className="text-xs text-muted">No warning events.</div>}
              {data.warnings.map((e: K8sObject) => (
                <button
                  key={e.metadata.uid}
                  onClick={() => onNavigate(kindKeyFor(e.involvedObject?.kind, e.involvedObject?.apiVersion), e.involvedObject?.namespace, e.involvedObject?.name)}
                  className="flex flex-col gap-0.5 border-b border-line/50 py-1.5 text-left text-xs hover:bg-hover/50"
                >
                  <div className="flex w-full items-center gap-2">
                    <span className="font-medium text-warn">{e.reason}</span>
                    <span className="min-w-0 flex-1 truncate text-muted">{e.involvedObject?.kind}/{e.involvedObject?.namespace ? `${e.involvedObject.namespace}/` : ""}{e.involvedObject?.name}</span>
                    {(e.count ?? 1) > 1 && <span className="text-faint">×{e.count}</span>}
                    <span className="shrink-0 tabular-nums text-faint">{age(eventTime(e), now)}</span>
                  </div>
                  <div className="line-clamp-2 text-fg/80">{e.message}</div>
                </button>
              ))}
            </div>
          </Card>
        </div>
      </div>
    </div>
  );
}

function NodeRow({ node, cpu, mem, onClick }: { node: K8sObject; cpu?: number; mem?: number; onClick: () => void }) {
  const st = nodeStatus(node);
  const ac = parseQty(node.status?.allocatable?.cpu);
  const am = parseQty(node.status?.allocatable?.memory);
  const bar = (v: number | undefined, max: number, label: string) => {
    const pct = v != null && max ? (v / max) * 100 : null;
    return (
      <div className="flex items-center gap-1.5" title={pct != null ? `${label} ${pct.toFixed(0)}%` : `${label} n/a`}>
        <div className="h-1.5 w-20 overflow-hidden rounded-full bg-hover">
          {pct != null && <div className={clsx("h-full rounded-full", pct > 90 ? "bg-danger" : pct > 75 ? "bg-warn" : "bg-info")} style={{ width: `${Math.min(100, pct)}%` }} />}
        </div>
        <span className="w-9 text-right tabular-nums text-muted">{pct != null ? `${pct.toFixed(0)}%` : "—"}</span>
      </div>
    );
  };
  return (
    <button onClick={onClick} className="grid grid-cols-[minmax(0,1fr)_auto_auto] items-center gap-3 rounded px-1 py-1 text-left text-xs hover:bg-hover/50">
      <span className="flex min-w-0 items-center gap-1.5">
        <span className={clsx("size-1.5 shrink-0 rounded-full", st.tone === "ok" ? "bg-ok" : st.tone === "warn" ? "bg-warn" : "bg-danger")} title={st.text} />
        <span className="truncate">{node.metadata.name}</span>
      </span>
      <span className="flex items-center gap-1"><Cpu className="size-3 text-faint" />{bar(cpu, ac, "CPU")}</span>
      <span className="flex items-center gap-1"><MemoryStick className="size-3 text-faint" />{bar(mem, am, "Memory")}</span>
    </button>
  );
}
