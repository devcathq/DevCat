import Editor from "@monaco-editor/react";
import clsx from "clsx";
import {
  ChevronDown, ChevronRight, Eye, EyeOff, FileCode2, Layers, Network, Pause, Play, RefreshCw, RotateCcw, Save,
  Scaling, ScrollText, SquareTerminal, Trash2, X,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { toast } from "sonner";
import { Badge, Button, ErrorBox, IconButton, Spinner, Spin } from "../../components/ui";
import { errMsg } from "../../lib/api";
import { k8s, type K8sObject } from "./api";
import { PortForwardDialog, ScaleDialog, useConfirm } from "./dialogs";
import {
  age, eventTime, fmtBytes, fmtCpu, labelSelectorString, nodeStatus, parseQty, podContainers, podStatus,
  selectorString, statusTone, ts, useNow,
} from "./format";
import { useAsync } from "./hooks";
import { StatusText, kindKeyFor, kindName } from "./resources";
import { POD_OWNERS } from "./rowActions";

export interface DrawerActions {
  logs: (pod: K8sObject, container?: string) => void;
  shell: (pod: K8sObject, container?: string) => void;
  /** Logs / shell for a workload (Deployment, StatefulSet, …): picks one of its pods. */
  workload: (type: "logs" | "shell", obj: K8sObject) => void;
  navigate: (kindKey: string, namespace: string | undefined, name: string) => void;
  browseCrd: (crd: K8sObject) => void;
}

const SCALABLE = new Set(["deployments", "statefulsets", "replicasets"]);
const RESTARTABLE = new Set(["deployments", "statefulsets", "daemonsets"]);

export default function DetailsDrawer({ connId, kindKey, obj, loading, readOnly, onClose, actions, width, setWidth, initialTab }: {
  connId: string;
  kindKey: string;
  obj: K8sObject | null;
  loading?: boolean;
  readOnly: boolean;
  onClose: () => void;
  actions: DrawerActions;
  width: number;
  setWidth: (w: number) => void;
  /** Tab to show when opened (e.g. "yaml" for Edit); re-applied whenever it changes. */
  initialTab?: { tab: "details" | "events" | "yaml"; nonce: number };
}) {
  const [tab, setTab] = useState<"details" | "events" | "yaml">(initialTab?.tab ?? "details");
  useEffect(() => { if (initialTab) setTab(initialTab.tab); }, [initialTab]);
  const [scaleOpen, setScaleOpen] = useState(false);
  const [pf, setPf] = useState<{ port?: number } | null>(null);
  const confirm = useConfirm();
  const uid = obj?.metadata.uid;
  // A different object: leave the YAML editor unless the caller asked for it.
  const lastUid = useRef(uid);
  useEffect(() => {
    if (lastUid.current !== uid && lastUid.current !== undefined) setTab((t) => (t === "yaml" && initialTab?.tab !== "yaml" ? "details" : t));
    lastUid.current = uid;
  }, [uid, initialTab]);

  const onDown = (e: React.MouseEvent) => {
    const start = { x: e.clientX, w: width };
    const move = (ev: MouseEvent) => setWidth(Math.max(380, Math.min(window.innerWidth * 0.85, start.w + start.x - ev.clientX)));
    const up = () => { window.removeEventListener("mousemove", move); window.removeEventListener("mouseup", up); };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };

  const ns = obj?.metadata.namespace;
  const name = obj?.metadata.name ?? "";
  const isPod = kindKey === "pods";

  const del = () => obj && confirm.ask({
    title: `Delete ${obj.kind}`,
    danger: true,
    confirmLabel: "Delete",
    body: <>Delete <b className="font-mono">{ns ? `${ns}/` : ""}{name}</b>? This cannot be undone.{obj.kind === "Namespace" && <div className="mt-2 text-danger">All resources in this namespace will be deleted.</div>}</>,
    run: async () => { await k8s.delete(connId, kindKey, ns, name); toast.success(`Deleted ${name}`); onClose(); },
  });
  const forceDel = () => obj && confirm.ask({
    title: "Force delete pod",
    danger: true,
    confirmLabel: "Force delete",
    body: <>Immediately remove <b className="font-mono">{ns}/{name}</b> without waiting for graceful termination?</>,
    run: async () => { await k8s.delete(connId, kindKey, ns, name, true); toast.success(`Deleted ${name}`); onClose(); },
  });
  const restart = () => obj && confirm.ask({
    title: `Restart ${obj.kind}`,
    confirmLabel: "Restart",
    body: <>Perform a rolling restart of <b className="font-mono">{ns}/{name}</b>? Pods will be replaced gradually.</>,
    run: async () => { await k8s.restart(connId, kindKey, ns, name); toast.success(`Restarting ${name}`); },
  });
  const suspend = () => obj && confirm.ask({
    title: obj.spec?.suspend ? "Resume CronJob" : "Suspend CronJob",
    confirmLabel: obj.spec?.suspend ? "Resume" : "Suspend",
    body: <>{obj.spec?.suspend ? "Resume" : "Suspend"} scheduling of <b className="font-mono">{ns}/{name}</b>?</>,
    run: async () => { await k8s.cronjobSuspend(connId, ns!, name, !obj.spec?.suspend); },
  });

  return (
    <div className="relative flex h-full shrink-0 flex-col border-l border-line bg-panel shadow-[-8px_0_24px_rgba(0,0,0,0.25)]" style={{ width }}>
      <div onMouseDown={onDown} className="absolute inset-y-0 -left-0.5 z-10 w-1 cursor-col-resize hover:bg-accent/60" />
      <div className="flex h-10 shrink-0 items-center gap-1 border-b border-line pl-3 pr-1.5">
        <div className="min-w-0 flex-1">
          <div className="truncate text-[13px] font-semibold" title={name}>
            <span className="text-muted">{obj?.kind ?? kindName(kindKey)}:</span> {name}
          </div>
        </div>
        {obj && (
          <>
            {isPod && <IconButton title="Logs" onClick={() => actions.logs(obj)}><ScrollText className="size-4" /></IconButton>}
            {POD_OWNERS.has(kindKey) && <IconButton title="Logs" onClick={() => actions.workload("logs", obj)}><ScrollText className="size-4" /></IconButton>}
            {POD_OWNERS.has(kindKey) && !readOnly && <IconButton title="Shell" onClick={() => actions.workload("shell", obj)}><SquareTerminal className="size-4" /></IconButton>}
            {isPod && !readOnly && <IconButton title="Shell" onClick={() => actions.shell(obj)}><SquareTerminal className="size-4" /></IconButton>}
            {(isPod || kindKey === "services") && !readOnly && <IconButton title="Port forward" onClick={() => setPf({})}><Network className="size-4" /></IconButton>}
            {SCALABLE.has(kindKey) && !readOnly && <IconButton title="Scale" onClick={() => setScaleOpen(true)}><Scaling className="size-4" /></IconButton>}
            {RESTARTABLE.has(kindKey) && !readOnly && <IconButton title="Restart" onClick={restart}><RotateCcw className="size-4" /></IconButton>}
            {kindKey === "cronjobs" && !readOnly && <IconButton title={obj.spec?.suspend ? "Resume" : "Suspend"} onClick={suspend}>{obj.spec?.suspend ? <Play className="size-4" /> : <Pause className="size-4" />}</IconButton>}
            {kindKey === "customresourcedefinitions" && <IconButton title="Browse instances" onClick={() => actions.browseCrd(obj)}><Layers className="size-4" /></IconButton>}
            {!readOnly && <IconButton title="Delete" className="hover:text-danger" onClick={del}><Trash2 className="size-4" /></IconButton>}
            {isPod && !readOnly && obj.metadata.deletionTimestamp && <Button size="sm" variant="danger" onClick={forceDel}>Force</Button>}
            <div className="mx-0.5 h-4 w-px bg-line" />
          </>
        )}
        <IconButton title="Close" onClick={onClose}><X className="size-4" /></IconButton>
      </div>
      <div className="flex h-8 shrink-0 items-end gap-3 border-b border-line px-3 text-xs">
        {(["details", "events", "yaml"] as const).map((t) => (
          <button key={t} onClick={() => setTab(t)} className={clsx("h-full border-b-2 px-0.5 capitalize", tab === t ? "border-accent text-fg" : "border-transparent text-muted hover:text-fg")}>
            {t === "yaml" ? "YAML" : t}
          </button>
        ))}
      </div>
      <div className="min-h-0 flex-1 overflow-hidden">
        {!obj ? (
          loading ? <div className="flex h-full items-center justify-center"><Spinner /></div> : <div className="p-4 text-xs text-muted">Object no longer exists.</div>
        ) : tab === "details" ? (
          <div className="h-full overflow-auto">
            <Summary connId={connId} kindKey={kindKey} obj={obj} actions={actions} readOnly={readOnly} onPortForward={(port) => setPf({ port })} />
          </div>
        ) : tab === "events" ? (
          <ObjectEvents connId={connId} kindKey={kindKey} obj={obj} />
        ) : (
          <YamlEditor connId={connId} kindKey={kindKey} obj={obj} readOnly={readOnly} />
        )}
      </div>
      <ScaleDialog open={scaleOpen} connId={connId} kind={kindKey} obj={obj} onClose={() => setScaleOpen(false)} />
      <PortForwardDialog open={!!pf} connId={connId} obj={obj} initialPort={pf?.port} onClose={() => setPf(null)} />
      {confirm.node}
    </div>
  );
}

// ---- building blocks ---------------------------------------------------------

function Section({ title, children, right, defaultOpen = true }: { title: string; children: ReactNode; right?: ReactNode; defaultOpen?: boolean }) {
  const [open, setOpen] = useState(defaultOpen);
  return (
    <section className="border-b border-line/60 px-3 py-2.5">
      <div className="mb-1.5 flex items-center gap-1">
        <button onClick={() => setOpen(!open)} className="flex items-center gap-1 text-[11px] font-semibold uppercase tracking-wider text-muted hover:text-fg">
          {open ? <ChevronDown className="size-3" /> : <ChevronRight className="size-3" />}{title}
        </button>
        <div className="flex-1" />
        {right}
      </div>
      {open && children}
    </section>
  );
}

function KV({ rows }: { rows: [string, ReactNode][] }) {
  const shown = rows.filter(([, v]) => v !== undefined && v !== null && v !== "" && !(Array.isArray(v) && !v.length));
  return (
    <div className="selectable grid grid-cols-[132px_minmax(0,1fr)] gap-x-3 gap-y-1 text-[12.5px]">
      {shown.map(([k, v]) => (
        <div key={k} className="contents">
          <div className="text-muted">{k}</div>
          <div className="min-w-0 break-words">{v}</div>
        </div>
      ))}
    </div>
  );
}

function Chips({ map, max = 50 }: { map?: Record<string, string>; max?: number }) {
  const entries = Object.entries(map ?? {});
  if (!entries.length) return null;
  return (
    <div className="flex flex-wrap gap-1">
      {entries.slice(0, max).map(([k, v]) => (
        <span key={k} className="selectable max-w-full truncate rounded bg-hover px-1.5 py-px font-mono text-[11px] text-fg/85" title={`${k}=${v}`}>
          {k}{v !== "" && <span className="text-muted">=</span>}{v}
        </span>
      ))}
    </div>
  );
}

function Annotations({ map }: { map?: Record<string, string> }) {
  const [open, setOpen] = useState(false);
  const entries = Object.entries(map ?? {});
  if (!entries.length) return null;
  return (
    <div className="flex flex-col gap-0.5">
      {(open ? entries : entries.slice(0, 4)).map(([k, v]) => (
        <div key={k} className="selectable font-mono text-[11px]">
          <span className="text-muted">{k}: </span>
          <span className={clsx("text-fg/85", !open && "line-clamp-1 break-all")}>{v}</span>
        </div>
      ))}
      {(entries.length > 4 || entries.some(([, v]) => v.length > 80)) && (
        <button className="self-start text-[11px] text-accent hover:underline" onClick={() => setOpen(!open)}>{open ? "Show less" : `Show all (${entries.length})`}</button>
      )}
    </div>
  );
}

function Conditions({ conditions }: { conditions?: K8sObject[] }) {
  const now = useNow();
  if (!conditions?.length) return null;
  return (
    <div className="flex flex-col gap-1">
      {conditions.map((c) => {
        const good = c.status === "True" ? !/Pressure|Unavailable|Failure|Failed|Disrupt/.test(c.type) : /Pressure|Unavailable/.test(c.type);
        return (
          <div key={c.type} className="flex items-start gap-2 text-[12px]" title={c.message}>
            <Badge tone={good ? "ok" : c.status === "Unknown" ? "warn" : "danger"}>{c.type}</Badge>
            <span className="min-w-0 flex-1 truncate text-muted">{c.reason ?? ""}{c.message ? ` — ${c.message}` : ""}</span>
            <span className="shrink-0 tabular-nums text-faint">{age(ts(c.lastTransitionTime ?? c.lastUpdateTime ?? c.lastHeartbeatTime), now)}</span>
          </div>
        );
      })}
    </div>
  );
}

function Mono({ children }: { children: ReactNode }) {
  return <span className="font-mono text-[12px]">{children}</span>;
}

// ---- summary --------------------------------------------------------------------

function Summary({ connId, kindKey, obj, actions, readOnly, onPortForward }: {
  connId: string; kindKey: string; obj: K8sObject; actions: DrawerActions; readOnly: boolean; onPortForward: (port: number) => void;
}) {
  const now = useNow();
  const m = obj.metadata;
  const created = ts(m.creationTimestamp);
  return (
    <>
      <Section title="Metadata">
        <KV rows={[
          ["Created", created ? <span>{age(created, now)} ago <span className="text-faint">({new Date(created).toLocaleString()})</span></span> : undefined],
          ["Name", <Mono>{m.name}</Mono>],
          ["Namespace", m.namespace ? <button className="text-accent hover:underline" onClick={() => actions.navigate("namespaces", undefined, m.namespace!)}>{m.namespace}</button> : undefined],
          ["UID", <Mono>{m.uid}</Mono>],
          ["Labels", <Chips map={m.labels} />],
          ["Annotations", <Annotations map={m.annotations} />],
          ["Controlled By", m.ownerReferences?.length ? (
            <div className="flex flex-col">
              {m.ownerReferences.map((r) => (
                <button key={r.uid} className="self-start text-left text-accent hover:underline" onClick={() => actions.navigate(kindKeyFor(r.kind, r.apiVersion), m.namespace, r.name)}>
                  {r.kind} {r.name}
                </button>
              ))}
            </div>
          ) : undefined],
          ["Finalizers", m.finalizers?.length ? <Mono>{m.finalizers.join(", ")}</Mono> : undefined],
        ]} />
      </Section>
      <KindSummary connId={connId} kindKey={kindKey} obj={obj} actions={actions} readOnly={readOnly} onPortForward={onPortForward} />
    </>
  );
}

function KindSummary({ connId, kindKey, obj, actions, readOnly, onPortForward }: {
  connId: string; kindKey: string; obj: K8sObject; actions: DrawerActions; readOnly: boolean; onPortForward: (port: number) => void;
}) {
  const s = obj.spec ?? {};
  const st = obj.status ?? {};
  switch (kindKey) {
    case "pods": {
      const ps = podStatus(obj);
      const { ready, total, restarts } = podContainers(obj);
      return (
        <>
          <Section title="Pod">
            <KV rows={[
              ["Status", <StatusText {...ps} />],
              ["Ready", `${ready}/${total}`],
              ["Restarts", String(restarts)],
              ["Node", s.nodeName ? <button className="text-accent hover:underline" onClick={() => actions.navigate("nodes", undefined, s.nodeName)}>{s.nodeName}</button> : undefined],
              ["Pod IP", <Mono>{(st.podIPs ?? []).map((p: K8sObject) => p.ip).join(", ") || st.podIP}</Mono>],
              ["Host IP", <Mono>{st.hostIP}</Mono>],
              ["QoS Class", st.qosClass],
              ["Service Account", s.serviceAccountName],
              ["Priority Class", s.priorityClassName],
              ["Restart Policy", s.restartPolicy],
              ["Node Selector", <Chips map={s.nodeSelector} />],
              ["Tolerations", s.tolerations?.length ? `${s.tolerations.length}` : undefined],
            ]} />
          </Section>
          <Section title="Conditions"><Conditions conditions={st.conditions} /></Section>
          {(s.initContainers ?? []).length > 0 && (
            <Section title="Init Containers" defaultOpen={false}>
              <Containers pod={obj} specs={s.initContainers} statuses={st.initContainerStatuses} actions={actions} readOnly={readOnly} onPortForward={onPortForward} />
            </Section>
          )}
          <Section title="Containers">
            <Containers pod={obj} specs={s.containers ?? []} statuses={st.containerStatuses} actions={actions} readOnly={readOnly} onPortForward={onPortForward} />
          </Section>
          {(s.volumes ?? []).length > 0 && (
            <Section title="Volumes" defaultOpen={false}>
              <div className="flex flex-col gap-1 text-[12px]">
                {s.volumes.map((v: K8sObject) => {
                  const type = Object.keys(v).find((k) => k !== "name") ?? "";
                  const ref = v.configMap?.name ?? v.secret?.secretName ?? v.persistentVolumeClaim?.claimName;
                  const target = v.configMap ? "configmaps" : v.secret ? "secrets" : v.persistentVolumeClaim ? "persistentvolumeclaims" : null;
                  return (
                    <div key={v.name} className="flex gap-2">
                      <Mono>{v.name}</Mono>
                      <span className="text-muted">{type}</span>
                      {ref && target && <button className="text-accent hover:underline" onClick={() => actions.navigate(target, obj.metadata.namespace, ref)}>{ref}</button>}
                    </div>
                  );
                })}
              </div>
            </Section>
          )}
        </>
      );
    }
    case "deployments":
    case "statefulsets":
    case "daemonsets":
    case "replicasets": {
      const rows: [string, ReactNode][] = kindKey === "daemonsets"
        ? [
          ["Desired", st.desiredNumberScheduled], ["Current", st.currentNumberScheduled], ["Ready", st.numberReady],
          ["Up-to-date", st.updatedNumberScheduled], ["Available", st.numberAvailable], ["Misscheduled", st.numberMisscheduled],
        ]
        : [
          ["Replicas", `${st.readyReplicas ?? 0} ready / ${s.replicas ?? 0} desired · ${st.updatedReplicas ?? 0} updated · ${st.availableReplicas ?? 0} available`],
        ];
      return (
        <>
          <Section title={kindName(kindKey)}>
            <KV rows={[
              ...rows.map(([k, v]) => [k, v == null ? undefined : String(v)] as [string, ReactNode]),
              ["Selector", <Mono>{labelSelectorString(s.selector)}</Mono>],
              ["Strategy", s.strategy?.type ?? s.updateStrategy?.type],
              ["Images", <div className="flex flex-col">{(s.template?.spec?.containers ?? []).map((c: K8sObject) => <Mono key={c.name}>{c.image}</Mono>)}</div>],
            ]} />
          </Section>
          <Section title="Conditions"><Conditions conditions={st.conditions} /></Section>
          <RelatedPods connId={connId} namespace={obj.metadata.namespace} selector={labelSelectorString(s.selector)} actions={actions} ownerUid={kindKey === "deployments" ? undefined : obj.metadata.uid} />
        </>
      );
    }
    case "jobs":
      return (
        <>
          <Section title="Job">
            <KV rows={[
              ["Completions", `${st.succeeded ?? 0}/${s.completions ?? 1}`],
              ["Parallelism", String(s.parallelism ?? 1)],
              ["Active / Failed", `${st.active ?? 0} / ${st.failed ?? 0}`],
              ["Started", st.startTime ? new Date(st.startTime).toLocaleString() : undefined],
              ["Completed", st.completionTime ? new Date(st.completionTime).toLocaleString() : undefined],
              ["Backoff Limit", s.backoffLimit != null ? String(s.backoffLimit) : undefined],
            ]} />
          </Section>
          <Section title="Conditions"><Conditions conditions={st.conditions} /></Section>
          <RelatedPods connId={connId} namespace={obj.metadata.namespace} selector={labelSelectorString(s.selector)} actions={actions} />
        </>
      );
    case "cronjobs":
      return (
        <Section title="CronJob">
          <KV rows={[
            ["Schedule", <Mono>{s.schedule}</Mono>],
            ["Time Zone", s.timeZone],
            ["Suspended", s.suspend ? "Yes" : "No"],
            ["Concurrency", s.concurrencyPolicy],
            ["Last Schedule", st.lastScheduleTime ? new Date(st.lastScheduleTime).toLocaleString() : undefined],
            ["Last Success", st.lastSuccessfulTime ? new Date(st.lastSuccessfulTime).toLocaleString() : undefined],
            ["Active Jobs", (st.active ?? []).length ? (
              <div className="flex flex-col">{st.active.map((j: K8sObject) => <button key={j.uid} className="self-start text-accent hover:underline" onClick={() => actions.navigate("jobs", j.namespace, j.name)}>{j.name}</button>)}</div>
            ) : undefined],
          ]} />
        </Section>
      );
    case "services":
      return (
        <>
          <Section title="Service">
            <KV rows={[
              ["Type", s.type],
              ["Cluster IP", <Mono>{(s.clusterIPs ?? [s.clusterIP]).join(", ")}</Mono>],
              ["External", <Mono>{(st.loadBalancer?.ingress ?? []).map((i: K8sObject) => i.ip || i.hostname).join(", ") || s.externalName}</Mono>],
              ["Session Affinity", s.sessionAffinity],
              ["Selector", <Chips map={s.selector} />],
            ]} />
          </Section>
          <Section title="Ports">
            <div className="flex flex-col gap-1 text-[12px]">
              {(s.ports ?? []).map((p: K8sObject) => (
                <div key={`${p.port}-${p.protocol}`} className="flex items-center gap-2">
                  <Mono>{p.name ? `${p.name}: ` : ""}{p.port}{p.nodePort ? `:${p.nodePort}` : ""}/{p.protocol ?? "TCP"} → {p.targetPort}</Mono>
                  {!readOnly && (p.protocol ?? "TCP") === "TCP" && s.selector && <Button size="sm" variant="ghost" onClick={() => onPortForward(p.port)}><Network className="size-3" />Forward</Button>}
                </div>
              ))}
            </div>
          </Section>
          {s.selector && <RelatedPods connId={connId} namespace={obj.metadata.namespace} selector={selectorString(s.selector)} actions={actions} />}
        </>
      );
    case "ingresses":
      return (
        <Section title="Rules">
          <div className="flex flex-col gap-2 text-[12px]">
            {(s.rules ?? []).map((r: K8sObject, i: number) => (
              <div key={i}>
                <div className="font-medium">{r.host ?? "*"}</div>
                {(r.http?.paths ?? []).map((p: K8sObject, j: number) => (
                  <div key={j} className="flex gap-2 pl-3">
                    <Mono>{p.path ?? "/"}</Mono><span className="text-muted">→</span>
                    {p.backend?.service ? (
                      <button className="text-accent hover:underline" onClick={() => actions.navigate("services", obj.metadata.namespace, p.backend.service.name)}>
                        {p.backend.service.name}:{p.backend.service.port?.number ?? p.backend.service.port?.name}
                      </button>
                    ) : <span className="text-muted">{JSON.stringify(p.backend)}</span>}
                  </div>
                ))}
              </div>
            ))}
            {(s.tls ?? []).length > 0 && <div className="text-muted">TLS: {(s.tls as K8sObject[]).map((t) => `${(t.hosts ?? []).join(", ")} (${t.secretName})`).join("; ")}</div>}
          </div>
        </Section>
      );
    case "configmaps":
      return <DataSection data={{ ...(obj.data ?? {}) }} binary={Object.keys(obj.binaryData ?? {})} />;
    case "secrets":
      return <SecretData connId={connId} obj={obj} />;
    case "nodes":
      return <NodeSummary connId={connId} obj={obj} actions={actions} />;
    case "persistentvolumeclaims":
      return (
        <Section title="Claim">
          <KV rows={[
            ["Status", <StatusText text={st.phase ?? ""} tone={statusTone(st.phase ?? "")} />],
            ["Volume", s.volumeName ? <button className="text-accent hover:underline" onClick={() => actions.navigate("persistentvolumes", undefined, s.volumeName)}>{s.volumeName}</button> : undefined],
            ["Capacity", st.capacity?.storage ?? s.resources?.requests?.storage],
            ["Access Modes", (s.accessModes ?? []).join(", ")],
            ["Storage Class", s.storageClassName],
            ["Volume Mode", s.volumeMode],
          ]} />
        </Section>
      );
    case "horizontalpodautoscalers":
      return (
        <>
          <Section title="Autoscaler">
            <KV rows={[
              ["Target", s.scaleTargetRef ? <button className="text-accent hover:underline" onClick={() => actions.navigate(kindKeyFor(s.scaleTargetRef.kind), obj.metadata.namespace, s.scaleTargetRef.name)}>{s.scaleTargetRef.kind}/{s.scaleTargetRef.name}</button> : undefined],
              ["Replicas", `${st.currentReplicas ?? 0} current · ${st.desiredReplicas ?? 0} desired · ${s.minReplicas ?? 1}–${s.maxReplicas}`],
            ]} />
          </Section>
          <Section title="Conditions"><Conditions conditions={st.conditions} /></Section>
        </>
      );
    case "rolebindings":
    case "clusterrolebindings":
      return (
        <Section title="Binding">
          <KV rows={[
            ["Role", obj.roleRef ? <button className="text-accent hover:underline" onClick={() => actions.navigate(obj.roleRef.kind === "ClusterRole" ? "clusterroles" : "roles", obj.metadata.namespace, obj.roleRef.name)}>{obj.roleRef.kind}/{obj.roleRef.name}</button> : undefined],
            ["Subjects", <div className="flex flex-col">{(obj.subjects ?? []).map((x: K8sObject, i: number) => <Mono key={i}>{x.kind} {x.namespace ? `${x.namespace}/` : ""}{x.name}</Mono>)}</div>],
          ]} />
        </Section>
      );
    case "roles":
    case "clusterroles":
      return (
        <Section title="Rules">
          <div className="flex flex-col gap-1.5 text-[12px]">
            {(obj.rules ?? []).map((r: K8sObject, i: number) => (
              <div key={i} className="rounded border border-line/60 bg-app/40 px-2 py-1">
                <div><span className="text-muted">verbs </span><Mono>{(r.verbs ?? []).join(", ")}</Mono></div>
                {r.resources && <div><span className="text-muted">resources </span><Mono>{r.resources.join(", ")}</Mono></div>}
                {r.apiGroups && <div><span className="text-muted">apiGroups </span><Mono>{r.apiGroups.map((g: string) => g || '""').join(", ")}</Mono></div>}
                {r.nonResourceURLs && <div><span className="text-muted">urls </span><Mono>{r.nonResourceURLs.join(", ")}</Mono></div>}
              </div>
            ))}
          </div>
        </Section>
      );
    case "customresourcedefinitions":
      return (
        <Section title="Definition">
          <KV rows={[
            ["Group", <Mono>{s.group}</Mono>],
            ["Kind", s.names?.kind],
            ["Plural", s.names?.plural],
            ["Short Names", (s.names?.shortNames ?? []).join(", ")],
            ["Scope", s.scope],
            ["Versions", (s.versions ?? []).map((v: K8sObject) => `${v.name}${v.storage ? " (storage)" : ""}${v.served ? "" : " (not served)"}`).join(", ")],
            ["Instances", <Button size="sm" onClick={() => actions.browseCrd(obj)}><Layers className="size-3" />Browse {s.names?.kind}</Button>],
          ]} />
        </Section>
      );
    default:
      return st.conditions ? <Section title="Conditions"><Conditions conditions={st.conditions} /></Section> : null;
  }
}

function Containers({ pod, specs, statuses, actions, readOnly, onPortForward }: {
  pod: K8sObject; specs: K8sObject[]; statuses?: K8sObject[]; actions: DrawerActions; readOnly: boolean; onPortForward: (port: number) => void;
}) {
  const now = useNow();
  return (
    <div className="flex flex-col gap-2.5">
      {specs.map((c) => {
        const s = statuses?.find((x) => x.name === c.name);
        const state = s?.state ? Object.keys(s.state)[0] : "waiting";
        const detail = s?.state?.[state] ?? {};
        const tone = s?.ready ? "ok" : state === "terminated" ? (detail.exitCode === 0 ? "info" : "danger") : "warn";
        const last = s?.lastState?.terminated;
        return (
          <div key={c.name} className="rounded-md border border-line/70 bg-app/40 p-2">
            <div className="mb-1.5 flex items-center gap-2">
              <span className={clsx("size-2 rounded-full", { "bg-ok": tone === "ok", "bg-info": tone === "info", "bg-danger": tone === "danger", "bg-warn": tone === "warn" })} />
              <span className="font-medium">{c.name}</span>
              <span className="text-xs text-muted">{state}{detail.reason ? ` · ${detail.reason}` : ""}{state === "running" && detail.startedAt ? ` · ${age(ts(detail.startedAt), now)}` : ""}</span>
              <div className="flex-1" />
              <IconButton title="Logs" onClick={() => actions.logs(pod, c.name)}><ScrollText className="size-3.5" /></IconButton>
              {!readOnly && state === "running" && <IconButton title="Shell" onClick={() => actions.shell(pod, c.name)}><SquareTerminal className="size-3.5" /></IconButton>}
            </div>
            <KV rows={[
              ["Image", <Mono>{c.image}</Mono>],
              ["Restarts", s ? String(s.restartCount ?? 0) : undefined],
              ["Last Terminated", last ? <span className="text-warn">{last.reason} (exit {last.exitCode}) · {age(ts(last.finishedAt), now)} ago</span> : undefined],
              ["Ports", (c.ports ?? []).length ? (
                <div className="flex flex-wrap gap-1">
                  {c.ports.map((p: K8sObject) => (
                    <button
                      key={`${p.containerPort}-${p.protocol}`}
                      disabled={readOnly || (p.protocol ?? "TCP") !== "TCP"}
                      onClick={() => onPortForward(p.containerPort)}
                      title={readOnly ? undefined : "Port forward"}
                      className="rounded bg-hover px-1.5 py-px font-mono text-[11px] enabled:hover:bg-accent/20 enabled:hover:text-accent"
                    >
                      {p.name ? `${p.name}:` : ""}{p.containerPort}/{p.protocol ?? "TCP"}
                    </button>
                  ))}
                </div>
              ) : undefined],
              ["Command", c.command ? <Mono>{[...(c.command ?? []), ...(c.args ?? [])].join(" ")}</Mono> : c.args ? <Mono>{c.args.join(" ")}</Mono> : undefined],
              ["Requests", c.resources?.requests ? <Mono>{Object.entries(c.resources.requests).map(([k, v]) => `${k}=${v}`).join(", ")}</Mono> : undefined],
              ["Limits", c.resources?.limits ? <Mono>{Object.entries(c.resources.limits).map(([k, v]) => `${k}=${v}`).join(", ")}</Mono> : undefined],
              ["Environment", (c.env ?? []).length || (c.envFrom ?? []).length ? <EnvList c={c} ns={pod.metadata.namespace} actions={actions} /> : undefined],
              ["Mounts", (c.volumeMounts ?? []).length ? (
                <div className="flex flex-col">{c.volumeMounts.map((v: K8sObject) => <Mono key={v.mountPath}>{v.mountPath} <span className="text-muted">← {v.name}{v.readOnly ? " (ro)" : ""}</span></Mono>)}</div>
              ) : undefined],
              ["Probes", [c.livenessProbe && "liveness", c.readinessProbe && "readiness", c.startupProbe && "startup"].filter(Boolean).join(", ")],
            ]} />
          </div>
        );
      })}
    </div>
  );
}

function EnvList({ c, ns, actions }: { c: K8sObject; ns?: string; actions: DrawerActions }) {
  const [open, setOpen] = useState(false);
  const env: K8sObject[] = c.env ?? [];
  const shown = open ? env : env.slice(0, 6);
  const ref = (kind: string, name: string, key?: string) => (
    <button className="text-accent hover:underline" onClick={() => actions.navigate(kind, ns, name)}>{name}{key ? `.${key}` : ""}</button>
  );
  return (
    <div className="flex flex-col font-mono text-[11.5px]">
      {shown.map((e) => (
        <div key={e.name} className="truncate">
          <span className="text-fg/85">{e.name}</span><span className="text-muted">=</span>
          {e.value !== undefined ? <span className="text-muted">{e.value}</span>
            : e.valueFrom?.secretKeyRef ? <><span className="text-faint">secret:</span>{ref("secrets", e.valueFrom.secretKeyRef.name, e.valueFrom.secretKeyRef.key)}</>
            : e.valueFrom?.configMapKeyRef ? <><span className="text-faint">configmap:</span>{ref("configmaps", e.valueFrom.configMapKeyRef.name, e.valueFrom.configMapKeyRef.key)}</>
            : e.valueFrom?.fieldRef ? <span className="text-faint">field:{e.valueFrom.fieldRef.fieldPath}</span>
            : e.valueFrom?.resourceFieldRef ? <span className="text-faint">resource:{e.valueFrom.resourceFieldRef.resource}</span>
            : <span className="text-faint">—</span>}
        </div>
      ))}
      {(c.envFrom ?? []).map((f: K8sObject, i: number) => (
        <div key={`from-${i}`} className="truncate">
          <span className="text-faint">envFrom {f.secretRef ? "secret:" : "configmap:"}</span>
          {f.secretRef ? ref("secrets", f.secretRef.name) : f.configMapRef ? ref("configmaps", f.configMapRef.name) : null}
          {f.prefix && <span className="text-faint"> (prefix {f.prefix})</span>}
        </div>
      ))}
      {env.length > 6 && <button className="self-start font-sans text-[11px] text-accent hover:underline" onClick={() => setOpen(!open)}>{open ? "Show less" : `Show all (${env.length})`}</button>}
    </div>
  );
}

function RelatedPods({ connId, namespace, selector, actions, ownerUid }: { connId: string; namespace?: string; selector: string; actions: DrawerActions; ownerUid?: string }) {
  const now = useNow();
  const { data, error } = useAsync(() => (selector ? k8s.list(connId, "pods", namespace, selector) : Promise.resolve([])), [connId, namespace, selector], 5000);
  const pods = (data ?? []).filter((p) => !ownerUid || p.metadata.ownerReferences?.some((r) => r.uid === ownerUid));
  return (
    <Section title={`Pods${data ? ` (${pods.length})` : ""}`}>
      {error ? <div className="text-xs text-danger">{error}</div> : !data ? <Spinner /> : !pods.length ? <div className="text-xs text-muted">No pods.</div> : (
        <div className="flex flex-col text-[12px]">
          {pods.sort((a, b) => a.metadata.name.localeCompare(b.metadata.name)).map((p) => {
            const st = podStatus(p);
            const c = podContainers(p);
            return (
              <div key={p.metadata.uid} className="group flex h-6 items-center gap-2 rounded px-1 hover:bg-hover">
                <button className="min-w-0 flex-1 truncate text-left text-accent hover:underline" onClick={() => actions.navigate("pods", p.metadata.namespace, p.metadata.name)}>{p.metadata.name}</button>
                <span className="w-10 text-right tabular-nums text-muted">{c.ready}/{c.total}</span>
                <span className="w-24 truncate text-right"><StatusText {...st} /></span>
                <span className="w-12 text-right tabular-nums text-muted">{age(ts(p.metadata.creationTimestamp), now)}</span>
                <IconButton title="Logs" className="opacity-0 group-hover:opacity-100" onClick={() => actions.logs(p)}><ScrollText className="size-3.5" /></IconButton>
              </div>
            );
          })}
        </div>
      )}
    </Section>
  );
}

function DataSection({ data, binary }: { data: Record<string, string>; binary?: string[] }) {
  const entries = Object.entries(data);
  return (
    <Section title={`Data (${entries.length + (binary?.length ?? 0)})`}>
      <div className="flex flex-col gap-2">
        {entries.map(([k, v]) => (
          <div key={k}>
            <div className="mb-0.5 font-mono text-[11.5px] text-muted">{k}</div>
            <pre className="selectable max-h-72 overflow-auto rounded border border-line/60 bg-app p-2 font-mono text-[11.5px] whitespace-pre-wrap break-all">{v}</pre>
          </div>
        ))}
        {binary?.map((k) => <div key={k} className="font-mono text-[11.5px] text-muted">{k} <span className="text-faint">(binary)</span></div>)}
        {!entries.length && !binary?.length && <div className="text-xs text-muted">No data.</div>}
      </div>
    </Section>
  );
}

function decodeB64(s: string): string {
  try {
    const bin = atob(s);
    const bytes = Uint8Array.from(bin, (ch) => ch.charCodeAt(0));
    return new TextDecoder("utf-8", { fatal: false }).decode(bytes);
  } catch {
    return s;
  }
}

function SecretData({ connId, obj }: { connId: string; obj: K8sObject }) {
  const [revealed, setRevealed] = useState<Record<string, string> | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => setRevealed(null), [obj.metadata.uid, obj.metadata.resourceVersion]);
  const keys = Object.keys(obj.data ?? {});
  const reveal = async () => {
    setBusy(true);
    try {
      const full = await k8s.get(connId, "secrets", obj.metadata.namespace, obj.metadata.name);
      const out: Record<string, string> = {};
      for (const [k, v] of Object.entries((full.data ?? {}) as Record<string, string>)) out[k] = decodeB64(v);
      setRevealed(out);
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <>
      <Section title="Secret"><KV rows={[["Type", <Mono>{obj.type}</Mono>]]} /></Section>
      <Section
        title={`Data (${keys.length})`}
        right={keys.length > 0 && (revealed
          ? <Button size="sm" variant="ghost" onClick={() => setRevealed(null)}><EyeOff className="size-3" />Hide</Button>
          : <Button size="sm" variant="ghost" loading={busy} onClick={reveal}><Eye className="size-3" />Reveal values</Button>)}
      >
        <div className="flex flex-col gap-2">
          {keys.map((k) => (
            <div key={k}>
              <div className="mb-0.5 font-mono text-[11.5px] text-muted">{k}</div>
              {revealed
                ? <pre className="selectable max-h-60 overflow-auto rounded border border-line/60 bg-app p-2 font-mono text-[11.5px] whitespace-pre-wrap break-all">{revealed[k]}</pre>
                : <div className="font-mono text-[11.5px] text-faint">••••••••</div>}
            </div>
          ))}
          {!keys.length && <div className="text-xs text-muted">No data.</div>}
        </div>
      </Section>
    </>
  );
}

function NodeSummary({ connId, obj, actions }: { connId: string; obj: K8sObject; actions: DrawerActions }) {
  const st = obj.status ?? {};
  const info = st.nodeInfo ?? {};
  const ns = nodeStatus(obj);
  const { data: pods } = useAsync(() => k8s.list(connId, "pods", null), [connId, obj.metadata.name], 15000);
  const onNode = useMemo(() => (pods ?? []).filter((p) => p.spec?.nodeName === obj.metadata.name && !["Succeeded", "Failed"].includes(p.status?.phase)), [pods, obj.metadata.name]);
  const req = onNode.reduce((a, p) => {
    for (const c of p.spec?.containers ?? []) { a.cpu += parseQty(c.resources?.requests?.cpu); a.mem += parseQty(c.resources?.requests?.memory); }
    return a;
  }, { cpu: 0, mem: 0 });
  const alloc = { cpu: parseQty(st.allocatable?.cpu), mem: parseQty(st.allocatable?.memory), pods: parseQty(st.allocatable?.pods) };
  return (
    <>
      <Section title="Node">
        <KV rows={[
          ["Status", <StatusText {...ns} />],
          ["Addresses", <div className="flex flex-col">{(st.addresses ?? []).map((a: K8sObject) => <Mono key={a.type + a.address}>{a.type}: {a.address}</Mono>)}</div>],
          ["OS", `${info.osImage ?? ""} (${info.operatingSystem}/${info.architecture})`],
          ["Kernel", info.kernelVersion],
          ["Runtime", info.containerRuntimeVersion],
          ["Kubelet", info.kubeletVersion],
          ["Pod CIDR", obj.spec?.podCIDR],
          ["Taints", (obj.spec?.taints ?? []).length ? <div className="flex flex-col">{obj.spec.taints.map((t: K8sObject, i: number) => <Mono key={i}>{t.key}{t.value ? `=${t.value}` : ""}:{t.effect}</Mono>)}</div> : undefined],
        ]} />
      </Section>
      <Section title="Capacity">
        <KV rows={[
          ["CPU", `${fmtCpu(alloc.cpu)} allocatable · ${fmtCpu(req.cpu)} requested${alloc.cpu ? ` (${Math.round((req.cpu / alloc.cpu) * 100)}%)` : ""}`],
          ["Memory", `${fmtBytes(alloc.mem)} allocatable · ${fmtBytes(req.mem)} requested${alloc.mem ? ` (${Math.round((req.mem / alloc.mem) * 100)}%)` : ""}`],
          ["Pods", pods ? `${onNode.length} / ${alloc.pods}` : undefined],
        ]} />
      </Section>
      <Section title="Conditions"><Conditions conditions={st.conditions} /></Section>
      <Section title={`Pods${pods ? ` (${onNode.length})` : ""}`} defaultOpen={false}>
        <div className="flex flex-col text-[12px]">
          {onNode.map((p) => (
            <div key={p.metadata.uid} className="flex h-6 items-center gap-2">
              <button className="min-w-0 flex-1 truncate text-left text-accent hover:underline" onClick={() => actions.navigate("pods", p.metadata.namespace, p.metadata.name)}>{p.metadata.namespace}/{p.metadata.name}</button>
              <span className="w-24 text-right"><StatusText {...podStatus(p)} /></span>
            </div>
          ))}
        </div>
      </Section>
    </>
  );
}

// ---- events tab -----------------------------------------------------------------

function ObjectEvents({ connId, kindKey, obj }: { connId: string; kindKey: string; obj: K8sObject }) {
  const now = useNow();
  const { data, error, loading, reload } = useAsync(() => k8s.eventsFor(connId, kindKey, obj.metadata.namespace, obj.metadata.name), [connId, kindKey, obj.metadata.uid], 10000);
  const evs = (data ?? []).slice().sort((a, b) => eventTime(b) - eventTime(a));
  return (
    <div className="flex h-full flex-col">
      <div className="flex h-8 shrink-0 items-center justify-between border-b border-line px-3 text-xs text-muted">
        <span>{evs.length} event{evs.length === 1 ? "" : "s"}</span>
        <IconButton title="Refresh" onClick={reload}><Spin icon={RefreshCw} spinning={loading} className="size-3.5" /></IconButton>
      </div>
      <div className="min-h-0 flex-1 overflow-auto">
        {error && <ErrorBox error={error} />}
        {!error && !loading && !evs.length && <div className="p-4 text-xs text-muted">No events. Events are kept by the cluster for about an hour.</div>}
        {evs.map((e) => (
          <div key={e.metadata.uid} className="border-b border-line/50 px-3 py-2 text-[12.5px]">
            <div className="flex items-center gap-2">
              <Badge tone={e.type === "Warning" ? "warn" : "default"}>{e.type}</Badge>
              <span className="font-medium">{e.reason}</span>
              <div className="flex-1" />
              {(e.count ?? 1) > 1 && <span className="text-xs text-muted">×{e.count}</span>}
              <span className="text-xs tabular-nums text-muted">{age(eventTime(e), now)} ago</span>
            </div>
            <div className="selectable mt-0.5 text-fg/85">{e.message}</div>
            <div className="mt-0.5 text-[11px] text-faint">{e.source?.component ?? e.reportingComponent}{e.source?.host ? ` · ${e.source.host}` : ""}</div>
          </div>
        ))}
      </div>
    </div>
  );
}

// ---- yaml tab -------------------------------------------------------------------

function YamlEditor({ connId, kindKey, obj, readOnly }: { connId: string; kindKey: string; obj: K8sObject; readOnly: boolean }) {
  const [original, setOriginal] = useState<string | null>(null);
  const [text, setText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [applying, setApplying] = useState(false);
  const confirm = useConfirm();
  const seenVersion = useRef<string | undefined>(undefined);
  const dirty = original !== null && text !== original;

  const load = async () => {
    setError(null);
    try {
      const y = await k8s.getYaml(connId, kindKey, obj.metadata.namespace, obj.metadata.name);
      setOriginal(y);
      setText(y);
      seenVersion.current = obj.metadata.resourceVersion;
    } catch (e) {
      setError(errMsg(e));
    }
  };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  useEffect(() => { setOriginal(null); load(); }, [connId, kindKey, obj.metadata.uid]);
  const changedRemotely = original !== null && obj.metadata.resourceVersion !== seenVersion.current;

  const apply = async () => {
    setApplying(true);
    try {
      const res = await k8s.applyYaml(connId, text);
      toast.success(res);
      await load();
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setApplying(false);
    }
  };

  return (
    <div className="flex h-full flex-col">
      <div className="flex h-8 shrink-0 items-center gap-1.5 border-b border-line px-2 text-xs">
        <FileCode2 className="size-3.5 text-muted" />
        <span className="text-muted">{readOnly ? "Read-only connection" : dirty ? "Modified" : "Edit and apply (server-side apply)"}</span>
        {changedRemotely && !dirty && <button className="text-accent hover:underline" onClick={load}>Object changed — reload</button>}
        {changedRemotely && dirty && <span className="text-warn">Object changed on the server since loaded</span>}
        <div className="flex-1" />
        <IconButton title="Reload" onClick={load}><RefreshCw className="size-3.5" /></IconButton>
        {!readOnly && (
          <>
            <Button size="sm" variant="ghost" disabled={!dirty} onClick={() => setText(original ?? "")}>Revert</Button>
            <Button size="sm" variant="primary" disabled={!dirty} loading={applying} onClick={() => confirm.ask({
              title: "Apply changes",
              confirmLabel: "Apply",
              body: <>Apply the edited YAML to <b className="font-mono">{obj.metadata.namespace ? `${obj.metadata.namespace}/` : ""}{obj.metadata.name}</b> using server-side apply (field manager <span className="font-mono">devcat</span>, conflicts forced)?</>,
              run: apply,
            })}><Save className="size-3" />Apply</Button>
          </>
        )}
      </div>
      {error && <ErrorBox error={error} />}
      <div className="min-h-0 flex-1">
        {original === null && !error ? <div className="flex h-full items-center justify-center"><Spinner /></div> : (
          <Editor
            language="yaml"
            theme="devcat"
            value={text}
            onChange={(v) => setText(v ?? "")}
            options={{
              readOnly, minimap: { enabled: false }, fontSize: 12, scrollBeyondLastLine: false, tabSize: 2,
              renderWhitespace: "boundary", wordWrap: "off", automaticLayout: true, fontFamily: "JetBrains Mono, SF Mono, Menlo, monospace",
            }}
          />
        )}
      </div>
      {confirm.node}
    </div>
  );
}
