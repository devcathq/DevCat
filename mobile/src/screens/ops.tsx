// Network (VPNs and tunnels running on the computer) and Cluster (Kubernetes: pods,
// deployments, nodes, pod logs / events / YAML).
import clsx from "clsx";
import { FileText, Minus, Network as NetworkIcon, Plus, RefreshCw, RotateCcw, Server, Trash2 } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { onEvent } from "../lib/rpc";
import { host, nav, toast, toastErr, useApp } from "../lib/store";
import type { ConnectionConfig, K8sObject, NetProfile, NetStatus, TrafficSample } from "../lib/types";
import { Badge, Btn, Card, Confirm, Dot, Empty, ErrorBox, IconBtn, Item, Loading, Note, Screen, Seg, Toggle, ago, useLoad } from "../ui";

// ------------------------------------------------------------------ network

const KIND: Record<string, string> = { openvpn: "OpenVPN", wireguard: "WireGuard", ssh: "SSH tunnel", k8s: "Kubernetes port-forward" };

function rate(bps: number): string {
  if (bps >= 1024 * 1024) return `${(bps / 1024 / 1024).toFixed(1)} MB/s`;
  if (bps >= 1024) return `${Math.round(bps / 1024)} KB/s`;
  return `${Math.round(bps)} B/s`;
}

function since(iso?: string | null): string {
  if (!iso) return "";
  const s = Math.max(0, Math.round((Date.now() - Date.parse(iso)) / 1000));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
}

function Bars({ samples }: { samples: TrafficSample[] }) {
  const last = samples.slice(-30);
  const max = Math.max(1, ...last.map((s) => s.rxRate + s.txRate));
  return (
    <div className="flex h-9 items-end gap-[2px]">
      {last.map((s) => <i key={s.at} className="flex-1 rounded-[1px] bg-info/60" style={{ height: `${Math.max(4, ((s.rxRate + s.txRate) / max) * 100)}%` }} />)}
    </div>
  );
}

export function Network() {
  const current = useApp((s) => s.current);
  const profiles = useLoad(() => host<NetProfile[]>("net_list"), [current]);
  const [status, setStatus] = useState<Record<string, NetStatus>>({});
  const [traffic, setTraffic] = useState<Record<string, TrafficSample[]>>({});
  const [busy, setBusy] = useState<string | null>(null);
  const readonly = useApp((s) => s.machines.find((m) => m.id === s.current)?.access === "readonly");

  useEffect(() => {
    let alive = true;
    const load = () => host<NetStatus[]>("net_status").then((l) => { if (alive) setStatus(Object.fromEntries(l.map((s) => [s.id, s]))); }).catch(() => {});
    void load();
    const t = setInterval(() => { if (!document.hidden) void load(); }, 10000);
    const off1 = onEvent<NetStatus>("net-status", (s) => setStatus((m) => ({ ...m, [s.id]: s })), current ?? undefined);
    const off2 = onEvent<TrafficSample>("net-traffic", (s) => setTraffic((m) => ({ ...m, [s.id]: [...(m[s.id] ?? []).slice(-59), s] })), current ?? undefined);
    return () => { alive = false; clearInterval(t); off1(); off2(); };
  }, [current]);
  useEffect(() => {
    for (const s of Object.values(status)) {
      if (s.state === "connected" && !traffic[s.id]) void host<TrafficSample[]>("net_traffic", { id: s.id }).then((l) => setTraffic((m) => ({ ...m, [s.id]: l.slice(-60) }))).catch(() => {});
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [status]);

  const toggle = async (p: NetProfile, on: boolean) => {
    setBusy(p.id);
    try {
      await host(on ? "net_connect" : "net_disconnect", { id: p.id });
    } catch (e) {
      toastErr(e);
    } finally {
      setBusy(null);
    }
  };

  return (
    <Screen title="VPN & tunnels" sub={`on ${useApp.getState().machines.find((m) => m.id === current)?.name ?? "the computer"}`} right={<IconBtn label="Refresh" onClick={profiles.reload}><RefreshCw className="size-4" /></IconBtn>}>
      {profiles.loading ? <Loading /> : profiles.error ? <ErrorBox error={profiles.error} onRetry={profiles.reload} /> : (profiles.data ?? []).length === 0 ? (
        <Empty icon={<NetworkIcon className="size-8" />} title="No VPNs or tunnels">Add them in DevCat on the computer (VPN & Tunnels).</Empty>
      ) : profiles.data!.map((p) => {
        const s = status[p.id];
        const st = s?.state ?? "disconnected";
        const on = st === "connected" || st === "connecting";
        const t = traffic[p.id] ?? [];
        const lastT = t[t.length - 1];
        return (
          <Card key={p.id} tone={st === "error" ? "danger" : undefined}>
            <div className="flex items-center gap-3">
              <div className="min-w-0 flex-1">
                <div className="truncate font-semibold">{p.name}</div>
                <div className={clsx("truncate text-[11.5px]", st === "error" ? "text-danger" : "text-muted")}>
                  {KIND[p.kind] ?? p.kind} · {st === "connected" ? `connected ${since(s?.since)}` : st === "error" ? `failed: ${s?.error ?? "error"}` : st}
                </div>
              </div>
              {busy === p.id || st === "connecting" || st === "disconnecting" ? <Badge tone="warn">{st === "disconnecting" ? "stopping…" : "connecting…"}</Badge> : null}
              <Toggle on={on} disabled={busy === p.id || readonly} onChange={(v) => void toggle(p, v)} />
            </div>
            {st === "connected" && (
              <div className="mt-2 space-y-1.5">
                {s?.detail && <div className="font-mono text-[11.5px] text-muted">{s.detail}</div>}
                {lastT && <div className="text-[12px]">↓ {rate(lastT.rxRate)} · ↑ {rate(lastT.txRate)}</div>}
                {t.length > 1 && <Bars samples={t} />}
              </div>
            )}
            <button className="mt-1.5 text-[12px] font-semibold text-accent" onClick={() => nav.push({ s: "netlog", id: p.id, name: p.name })}>View log ›</button>
          </Card>
        );
      })}
      <Note>These run on the computer, not on this phone. Turning one on here connects the computer. VPNs without DevCat's passwordless helper ask for the admin password on the computer.</Note>
    </Screen>
  );
}

export function NetLog({ id, name }: { id: string; name: string }) {
  const [lines, setLines] = useState<string[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const end = useRef<HTMLDivElement>(null);
  useEffect(() => {
    host<string[]>("net_logs", { id }).then(setLines).catch((e) => setError(String(e)));
    return onEvent<{ id: string; lines: string[] }>("net-log", (p) => { if (p.id === id) setLines((l) => [...(l ?? []), ...p.lines].slice(-2000)); });
  }, [id]);
  useEffect(() => { end.current?.scrollIntoView(); }, [lines]);
  return (
    <Screen title={name} sub="Log">
      {error && <ErrorBox error={error} />}
      {!lines ? <Loading /> : lines.length === 0 ? <Empty title="Nothing logged yet" /> : (
        <pre className="selectable rounded-xl border border-line bg-[#0a0b0d] p-2.5 font-mono text-[10.5px] leading-relaxed break-all whitespace-pre-wrap text-[#cfd3da]">{lines.join("\n")}</pre>
      )}
      <div ref={end} />
    </Screen>
  );
}

// ------------------------------------------------------------------ cluster

type K8sTab = "pods" | "deployments" | "nodes";

function podState(p: K8sObject): { text: string; tone: "ok" | "warn" | "danger" | "off"; restarts: number } {
  const cs: any[] = p.status?.containerStatuses ?? [];
  const restarts = cs.reduce((n, c) => n + (c.restartCount ?? 0), 0);
  const waiting = cs.find((c) => c.state?.waiting)?.state.waiting.reason;
  const terminated = cs.find((c) => c.state?.terminated)?.state.terminated.reason;
  if (p.metadata && (p as any).metadata.deletionTimestamp) return { text: "Terminating", tone: "warn", restarts };
  if (waiting) return { text: waiting, tone: /BackOff|Err|Invalid/.test(waiting) ? "danger" : "warn", restarts };
  const phase = p.status?.phase ?? "Unknown";
  if (phase === "Running") return { text: cs.every((c) => c.ready) ? "Running" : "Running (not ready)", tone: cs.every((c) => c.ready) ? "ok" : "warn", restarts };
  if (phase === "Succeeded") return { text: terminated ?? "Completed", tone: "off", restarts };
  if (phase === "Failed") return { text: terminated ?? "Failed", tone: "danger", restarts };
  return { text: phase, tone: "warn", restarts };
}

const age = (ts?: string) => (ts ? ago(ts).replace(" ago", "") : "");

export function Cluster() {
  const current = useApp((s) => s.current);
  const readonly = useApp((s) => s.machines.find((m) => m.id === s.current)?.access === "readonly");
  const conns = useLoad(() => host<ConnectionConfig[]>("list_connections").then((l) => l.filter((c) => c.kind === "kubernetes")), [current]);
  const [connId, setConnId] = useState<string>(() => localStorage.getItem("devcat.mobile.k8s") ?? "");
  const [ns, setNs] = useState<string>(() => localStorage.getItem("devcat.mobile.k8s.ns") ?? "");
  const [tab, setTab] = useState<K8sTab>("pods");
  const [scale, setScale] = useState<{ name: string; ns: string; from: number; to: number } | null>(null);
  useEffect(() => {
    const list = conns.data ?? [];
    if (list.length && !list.some((c) => c.id === connId)) setConnId(list[0].id);
  }, [conns.data, connId]);
  const connected = useLoad(() => (connId ? host("k8s_connect", { connId }) : Promise.resolve(null)), [connId]);
  const namespaces = useLoad(() => (connId && connected.data !== undefined && !connected.error ? host<string[]>("k8s_namespaces", { connId }) : Promise.resolve([] as string[])), [connId, connected.data, connected.error]);
  const items = useLoad(
    () => (connId && !connected.error && connected.data !== undefined ? host<K8sObject[]>("k8s_list", { connId, kind: tab, namespace: tab === "nodes" ? null : ns || null }) : Promise.resolve([] as K8sObject[])),
    [connId, ns, tab, connected.data, connected.error], 5000,
  );
  useEffect(() => { localStorage.setItem("devcat.mobile.k8s", connId); }, [connId]);
  useEffect(() => { localStorage.setItem("devcat.mobile.k8s.ns", ns); }, [ns]);
  const sorted = useMemo(() => [...(items.data ?? [])].sort((a, b) => {
    if (tab !== "pods") return a.metadata.name.localeCompare(b.metadata.name);
    const rank = (p: K8sObject) => ({ danger: 0, warn: 1, ok: 2, off: 3 })[podState(p).tone];
    return rank(a) - rank(b) || a.metadata.name.localeCompare(b.metadata.name);
  }), [items.data, tab]);

  const act = async (cmd: string, args: Record<string, unknown>, done: string) => {
    try { await host(cmd, { connId, ...args }); toast(done, "ok"); items.reload(); } catch (e) { toastErr(e); }
  };

  if (conns.loading) return <Screen title="Cluster"><Loading /></Screen>;
  if ((conns.data ?? []).length === 0) {
    return <Screen title="Cluster"><Empty icon={<Server className="size-8" />} title="No Kubernetes connections">Add a cluster in DevCat on the computer; it shows up here.</Empty></Screen>;
  }
  return (
    <Screen title={
      <select className="max-w-full bg-transparent text-[17px] font-semibold" value={connId} onChange={(e) => { setConnId(e.target.value); setNs(""); }}>
        {conns.data!.map((c) => <option key={c.id} value={c.id}>{c.name}</option>)}
      </select>
    } right={<IconBtn label="Refresh" onClick={items.reload}><RefreshCw className="size-4" /></IconBtn>}>
      {connected.error && <ErrorBox error={connected.error} onRetry={connected.reload} />}
      {tab !== "nodes" && (
        <select className="h-10 w-full rounded-xl border border-line bg-panel px-3 text-[13px]" value={ns} onChange={(e) => setNs(e.target.value)}>
          <option value="">All namespaces</option>
          {(namespaces.data ?? []).map((n) => <option key={n} value={n}>{n}</option>)}
        </select>
      )}
      <Seg value={tab} onChange={setTab} options={[{ value: "pods", label: "Pods" }, { value: "deployments", label: "Deployments" }, { value: "nodes", label: "Nodes" }]} />
      {items.loading && !items.data ? <Loading /> : items.error ? <ErrorBox error={items.error} onRetry={items.reload} /> : sorted.length === 0 ? <Empty title={`No ${tab}`} /> : null}
      {tab === "pods" && sorted.map((p) => {
        const s = podState(p);
        return (
          <Item key={`${p.metadata.namespace}/${p.metadata.name}`} tone={s.tone === "danger" ? "danger" : undefined}
            onClick={() => nav.push({ s: "pod", connId, namespace: p.metadata.namespace ?? "default", name: p.metadata.name })}
            icon={<Dot tone={s.tone} />}
            title={<span className="font-mono text-[13px]">{p.metadata.name}</span>}
            meta={<span className={s.tone === "danger" ? "text-danger" : s.tone === "warn" ? "text-warn" : ""}>{s.text} · {s.restarts} restart{s.restarts === 1 ? "" : "s"} · {age(p.metadata.creationTimestamp)}{!ns && p.metadata.namespace ? ` · ${p.metadata.namespace}` : ""}</span>}
            chevron />
        );
      })}
      {tab === "deployments" && sorted.map((d) => {
        const want = d.spec?.replicas ?? 0;
        const ready = d.status?.readyReplicas ?? 0;
        const n = d.metadata.namespace ?? "default";
        return (
          <Card key={`${n}/${d.metadata.name}`} tone={ready < want ? "warn" : undefined}>
            <div className="flex items-center gap-2">
              <div className="min-w-0 flex-1">
                <div className="truncate font-mono text-[13px] font-semibold">{d.metadata.name}</div>
                <div className="text-[11.5px] text-muted">{ready} / {want} ready{!ns ? ` · ${n}` : ""}</div>
              </div>
              {!readonly && (
                <>
                  <IconBtn label="Scale down" onClick={() => setScale({ name: d.metadata.name, ns: n, from: want, to: Math.max(0, want - 1) })}><Minus className="size-4" /></IconBtn>
                  <b className="w-5 text-center tabular-nums">{want}</b>
                  <IconBtn label="Scale up" onClick={() => setScale({ name: d.metadata.name, ns: n, from: want, to: want + 1 })}><Plus className="size-4" /></IconBtn>
                  <IconBtn label="Restart" onClick={() => void act("k8s_restart", { kind: "deployments", namespace: n, name: d.metadata.name }, `Restarting ${d.metadata.name}`)}><RotateCcw className="size-4" /></IconBtn>
                </>
              )}
            </div>
          </Card>
        );
      })}
      {tab === "nodes" && sorted.map((node) => {
        const ready = (node.status?.conditions ?? []).find((c: any) => c.type === "Ready")?.status === "True";
        const roles = Object.keys(node.metadata.labels ?? {}).filter((l) => l.startsWith("node-role.kubernetes.io/")).map((l) => l.split("/")[1]).join(", ");
        return (
          <Item key={node.metadata.name} icon={<Dot tone={ready ? "ok" : "danger"} />} title={<span className="font-mono text-[13px]">{node.metadata.name}</span>}
            meta={`${ready ? "Ready" : "Not ready"}${roles ? ` · ${roles}` : ""} · ${node.status?.nodeInfo?.kubeletVersion ?? ""} · ${age(node.metadata.creationTimestamp)}`} />
        );
      })}
      <Confirm open={!!scale} title={`Scale ${scale?.name}?`} confirm={`Scale to ${scale?.to}`} danger={scale?.to === 0}
        onCancel={() => setScale(null)} onConfirm={() => { const s = scale!; setScale(null); void act("k8s_scale", { kind: "deployments", namespace: s.ns, name: s.name, replicas: s.to }, `Scaled ${s.name} to ${s.to}`); }}>
        From <b>{scale?.from}</b> to <b>{scale?.to}</b> replicas.{scale?.to === 0 ? " Nothing will be running." : ""}
      </Confirm>
    </Screen>
  );
}

export function Pod({ connId, namespace, name }: { connId: string; namespace: string; name: string }) {
  const readonly = useApp((s) => s.machines.find((m) => m.id === s.current)?.access === "readonly");
  const pod = useLoad(() => host<K8sObject>("k8s_get", { connId, kind: "pods", namespace, name }), [connId, namespace, name], 10000);
  const [tab, setTab] = useState<"logs" | "events" | "yaml">("logs");
  const containers: string[] = (pod.data?.spec?.containers ?? []).map((c: any) => c.name);
  const [container, setContainer] = useState<string>("");
  const [previous, setPrevious] = useState(false);
  const [lines, setLines] = useState<string[]>([]);
  const [ended, setEnded] = useState<string | null>(null);
  const [del, setDel] = useState(false);
  const end = useRef<HTMLDivElement>(null);
  const follow = useRef(true);
  useEffect(() => { if (!container && containers[0]) setContainer(containers[0]); }, [containers, container]);

  useEffect(() => {
    if (tab !== "logs" || !container) return;
    let stream: string | null = null;
    let alive = true;
    setLines([]);
    setEnded(null);
    const off1 = onEvent<{ streamId: string; lines: string[] }>("k8s-log", (p) => { if (p.streamId === stream) setLines((l) => [...l, ...p.lines].slice(-3000)); });
    const off2 = onEvent<{ streamId: string; error?: string | null }>("k8s-log-end", (p) => { if (p.streamId === stream) setEnded(p.error ?? "Log stream ended"); });
    host<string>("k8s_logs_start", { connId, namespace, pod: name, container, tailLines: 300, follow: !previous, previous, timestamps: false })
      .then((id) => { if (alive) stream = id; else void host("k8s_stream_stop", { streamId: id }).catch(() => {}); })
      .catch((e) => setEnded(String(e)));
    return () => { alive = false; off1(); off2(); if (stream) void host("k8s_stream_stop", { streamId: stream }).catch(() => {}); };
  }, [tab, container, previous, connId, namespace, name]);
  useEffect(() => { if (follow.current) end.current?.scrollIntoView(); }, [lines]);

  const events = useLoad(() => (tab === "events" ? host<any[]>("k8s_events_for", { connId, kind: "pods", namespace, name }) : Promise.resolve([])), [tab]);
  const yaml = useLoad(() => (tab === "yaml" ? host<string>("k8s_get_yaml", { connId, kind: "pods", namespace, name }) : Promise.resolve("")), [tab]);
  const s = pod.data ? podState(pod.data) : null;

  return (
    <Screen title={<span className="font-mono text-[15px]">{name}</span>} sub={s ? <span className={s.tone === "danger" ? "text-danger" : s.tone === "warn" ? "text-warn" : "text-ok"}>{s.text} · {s.restarts} restarts · {namespace}</span> : namespace}>
      <Seg value={tab} onChange={setTab} options={[{ value: "logs", label: "Logs" }, { value: "events", label: "Events" }, { value: "yaml", label: "YAML" }]} />
      {tab === "logs" && (
        <>
          <div className="flex gap-1.5 overflow-x-auto">
            <button onClick={() => setPrevious(false)} className={clsx("shrink-0 rounded-full px-3 py-1 text-[12px]", !previous ? "bg-accent/15 text-accent" : "bg-hover text-muted")}>● Follow</button>
            <button onClick={() => setPrevious(true)} className={clsx("shrink-0 rounded-full px-3 py-1 text-[12px]", previous ? "bg-accent/15 text-accent" : "bg-hover text-muted")}>Previous run</button>
            {containers.length > 1 && (
              <select className="shrink-0 rounded-full bg-hover px-3 py-1 text-[12px]" value={container} onChange={(e) => setContainer(e.target.value)}>
                {containers.map((c) => <option key={c} value={c}>container: {c}</option>)}
              </select>
            )}
          </div>
          <div className="min-h-64 rounded-xl border border-line bg-[#0a0b0d] p-2.5" onScroll={(e) => { const el = e.currentTarget; follow.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40; }}>
            {lines.length === 0 && !ended ? <Loading text="Waiting for logs…" /> : (
              <pre className="selectable font-mono text-[10.5px] leading-relaxed break-all whitespace-pre-wrap text-[#cfd3da]">{lines.map((l) => (/\b(error|fatal|panic|exception)\b/i.test(l) ? `\u0000${l}` : l)).join("\n").split("\n").map((l, i) => l.startsWith("\u0000") ? <span key={i} className="text-danger">{l.slice(1)}{"\n"}</span> : <span key={i}>{l}{"\n"}</span>)}</pre>
            )}
            {ended && <div className="mt-2 text-[11.5px] text-muted">{ended}</div>}
            <div ref={end} />
          </div>
        </>
      )}
      {tab === "events" && (events.loading ? <Loading /> : events.error ? <ErrorBox error={events.error} /> : (events.data ?? []).length === 0 ? <Empty title="No recent events" /> : events.data!.map((e, i) => (
        <Card key={i} tone={e.type === "Warning" ? "warn" : undefined}>
          <div className="flex items-center gap-2 text-[12px]"><b className="flex-1">{e.reason}</b><span className="text-muted">{ago(e.lastTimestamp ?? e.eventTime ?? e.metadata?.creationTimestamp)}{e.count > 1 ? ` · ×${e.count}` : ""}</span></div>
          <div className="mt-0.5 text-[12px] text-muted">{e.message}</div>
        </Card>
      )))}
      {tab === "yaml" && (yaml.loading ? <Loading /> : yaml.error ? <ErrorBox error={yaml.error} /> : <pre className="selectable overflow-x-auto rounded-xl border border-line bg-panel p-2.5 font-mono text-[10.5px]">{yaml.data}</pre>)}
      {!readonly && (
        <div className="flex gap-2 pt-1">
          <Btn className="flex-1" variant="danger" onClick={() => setDel(true)}><Trash2 className="size-4" />Delete pod</Btn>
        </div>
      )}
      <Note><FileText className="mr-1 inline size-3.5" />Pods managed by a deployment come back by themselves after Delete; that's how you restart one.</Note>
      <Confirm open={del} danger title={`Delete ${name}?`} confirm="Delete" typeToConfirm={name.split("-")[0]} onCancel={() => setDel(false)}
        onConfirm={() => { setDel(false); void host("k8s_delete", { connId, kind: "pods", namespace, name }).then(() => { toast("Pod deleted", "ok"); nav.back(); }).catch(toastErr); }}>
        In namespace <b>{namespace}</b>. If it belongs to a deployment, a new pod replaces it.
      </Confirm>
    </Screen>
  );
}
