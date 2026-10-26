// Docker engine tab: containers (grouped by Compose project, with CPU and memory, logs, a shell,
// start / stop / restart / remove), images, volumes and networks, through the `docker` CLI
// (src-tauri/src/infra/docker.rs). A read-only connection only looks.
import clsx from "clsx";
import { listen } from "@tauri-apps/api/event";
import { homeDir } from "@tauri-apps/api/path";
import {
  Boxes, ChevronRight, CircleStop, Copy, FileJson, HardDrive, Layers, MoreHorizontal, Network, Pause, Play, RefreshCw, RotateCw, ScrollText, Search, Skull, SquareTerminal, Trash2, X,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState, type MouseEvent, type ReactNode } from "react";
import { toast } from "sonner";
import { Button, IconButton, Input, Select, Spinner } from "../../components/ui";
import { errMsg } from "../../lib/api";
import { useConnections } from "../../store/connections";
import { openTab, useTabs } from "../../store/tabs";
import type { Tab } from "../../tabs/types";
import { useContextMenu, type MenuItem } from "../connections/ContextMenu";
import { confirmDialog } from "../connections/dialogs";
import { docker, type ContainerAction, type DockerContainer, type DockerImage, type DockerInfo, type DockerObject, type DockerStats } from "./api";

type View = "containers" | "images" | "volumes" | "networks";
const NAV: { id: View; label: string; icon: ReactNode }[] = [
  { id: "containers", label: "Containers", icon: <Boxes className="size-3.5" /> },
  { id: "images", label: "Images", icon: <Layers className="size-3.5" /> },
  { id: "volumes", label: "Volumes", icon: <HardDrive className="size-3.5" /> },
  { id: "networks", label: "Networks", icon: <Network className="size-3.5" /> },
];
const INTERVALS: [number, string][] = [[0, "Off"], [3000, "3s"], [5000, "5s"], [10000, "10s"], [30000, "30s"]];

/** What the bottom panel shows: a container's logs, or anything's `inspect`. */
type Panel = { kind: "logs"; id: string; name: string } | { kind: "inspect"; obj: DockerObject; id: string; name: string };

interface Ctx {
  connId: string;
  readOnly: boolean;
  filter: string;
  tick: number;
  refresh: () => void;
  show: (p: Panel) => void;
}

export default function DockerTab({ tab }: { tab: Extract<Tab, { type: "docker" }> }) {
  const profile = useConnections((s) => s.connections.find((c) => c.id === tab.connId));
  const [view, setView] = useState<View>("containers");
  const [filter, setFilter] = useState("");
  const [intervalMs, setIntervalMs] = useState(5000);
  const [tick, setTick] = useState(0);
  const [info, setInfo] = useState<DockerInfo | null>(null);
  const [infoError, setInfoError] = useState<string | null>(null);
  const [panel, setPanel] = useState<Panel | null>(null);
  const refresh = useCallback(() => setTick((t) => t + 1), []);
  // each refresh runs `docker`: not while the tab is in the background, and at once when it's back
  const active = useTabs((s) => s.activeId === tab.id);
  const shown = useRef(active);
  useEffect(() => {
    if (active && !shown.current) refresh();
    shown.current = active;
    if (!intervalMs || !active) return;
    const h = window.setInterval(refresh, intervalMs);
    return () => window.clearInterval(h);
  }, [intervalMs, refresh, active]);
  // the engine, now and then (it's also the "is Docker running" check)
  useEffect(() => {
    let live = true;
    docker.info(tab.connId).then((i) => { if (live) { setInfo(i); setInfoError(null); } }, (e) => { if (live) setInfoError(errMsg(e)); });
    return () => { live = false; };
  }, [tab.connId, Math.floor(tick / 6)]);

  const ctx: Ctx = { connId: tab.connId, readOnly: profile?.readOnly ?? false, filter: filter.trim().toLowerCase(), tick, refresh, show: setPanel };
  return (
    <div className="flex h-full min-h-0 flex-col bg-app">
      <div className="flex h-9 shrink-0 items-center gap-2 border-b border-line bg-panel px-2">
        <span className="truncate text-xs font-semibold text-fg/90">{profile?.name ?? "Docker"}</span>
        {profile?.readOnly && <span className="rounded bg-warn/15 px-1.5 text-[10px] font-medium text-warn">read-only</span>}
        {/* the list below shows the whole error */}
        {infoError ? <span className="truncate text-[11px] text-danger" title={infoError}>Can't reach the engine</span> : info && (
          <span className="truncate text-[11px] text-faint" title={info.name}>
            Docker {info.version} · {info.os} · {info.cpus} CPUs · {fmtBytes(info.memory)} · {info.running}/{info.containers} running · {info.images} images
          </span>
        )}
        <div className="mx-1 h-4 w-px bg-line" />
        <div className="relative w-64">
          <Search className="pointer-events-none absolute left-2 top-1/2 size-3 -translate-y-1/2 text-faint" />
          <Input className="h-6 pl-6 pr-6 text-xs" placeholder={`Filter ${view}`} value={filter} onChange={(e) => setFilter(e.target.value)} onKeyDown={(e) => e.key === "Escape" && setFilter("")} />
          {filter && <IconButton title="Clear" className="absolute right-1 top-1/2 size-5 -translate-y-1/2" onClick={() => setFilter("")}><X className="size-3" /></IconButton>}
        </div>
        <div className="flex-1" />
        <label className="flex items-center gap-1.5 text-[11px] text-muted">
          Refresh
          <Select className="h-6 !w-20 text-xs" value={intervalMs} onChange={(e) => setIntervalMs(Number(e.target.value))}>
            {INTERVALS.map(([ms, l]) => <option key={ms} value={ms}>{l}</option>)}
          </Select>
        </label>
        <IconButton title="Refresh now" onClick={refresh}><RefreshCw className="size-3.5" /></IconButton>
      </div>
      <div className="flex min-h-0 flex-1">
        <nav className="flex w-40 shrink-0 flex-col gap-px border-r border-line bg-panel p-1.5">
          {NAV.map((n) => (
            <button key={n.id} onClick={() => { setView(n.id); setFilter(""); }}
              className={clsx("flex h-7 items-center gap-2 rounded px-2 text-left text-xs", n.id === view ? "bg-accent/15 font-medium text-accent" : "text-muted hover:bg-hover hover:text-fg")}>
              {n.icon}{n.label}
            </button>
          ))}
          <div className="flex-1" />
          <div className="px-2 pb-1 text-[10px] leading-snug text-faint">{intervalMs ? `Auto-refresh every ${intervalMs / 1000}s` : "Auto-refresh off"}</div>
        </nav>
        <main className="flex min-h-0 min-w-0 flex-1 flex-col">
          <div className={clsx("min-h-0 overflow-auto", panel ? "basis-[55%]" : "flex-1")}>
            {view === "containers" && <ContainersView ctx={ctx} />}
            {view === "images" && <ImagesView ctx={ctx} />}
            {view === "volumes" && <VolumesView ctx={ctx} />}
            {view === "networks" && <NetworksView ctx={ctx} />}
          </div>
          {panel && (
            <div className="flex min-h-0 basis-[45%] flex-col border-t border-line">
              {panel.kind === "logs" ? <LogsPanel key={panel.id} connId={tab.connId} id={panel.id} name={panel.name} onClose={() => setPanel(null)} />
                : <InspectPanel key={`${panel.obj}:${panel.id}`} connId={tab.connId} obj={panel.obj} id={panel.id} name={panel.name} onClose={() => setPanel(null)} />}
            </div>
          )}
        </main>
      </div>
    </div>
  );
}

function fmtBytes(n: number) {
  if (!n) return "0 B";
  const u = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.min(u.length - 1, Math.floor(Math.log(n) / Math.log(1024)));
  return `${(n / 1024 ** i).toFixed(i ? 1 : 0)} ${u[i]}`;
}

/** Load a list on every refresh; keeps the last one while the next loads. */
function useList<T>(load: () => Promise<T[]>, deps: unknown[]) {
  const [list, setList] = useState<T[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    load().then((l) => { if (live) { setList(l); setError(null); } }, (e) => { if (live) setError(errMsg(e)); });
    return () => { live = false; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
  return { list, error };
}

const CONTAINER_COLS = "grid-cols-[minmax(160px,1.4fr)_minmax(120px,1fr)_minmax(120px,1fr)_minmax(90px,1fr)_70px_90px_112px]";
const HEAD = "grid h-7 items-center gap-2 border-b border-line bg-panel px-3 text-[11px] font-semibold tracking-wide text-muted uppercase";
const STATE_DOT: Record<string, string> = { running: "bg-ok", paused: "bg-warn", restarting: "bg-accent", dead: "bg-danger", created: "bg-faint", exited: "bg-faint" };

function ContainersView({ ctx }: { ctx: Ctx }) {
  const { list, error } = useList(() => docker.containers(ctx.connId), [ctx.connId, ctx.tick]);
  const anyRunning = !!list?.some((c) => c.state === "running");
  const [stats, setStats] = useState<Record<string, DockerStats>>({});
  useEffect(() => {
    if (!anyRunning) return setStats({});
    let live = true;
    docker.stats(ctx.connId).then((s) => live && setStats(Object.fromEntries(s.map((x) => [x.id, x]))), () => {});
    return () => { live = false; };
  }, [ctx.connId, ctx.tick, anyRunning]);
  const [runningOnly, setRunningOnly] = useState(false);
  const [closed, setClosed] = useState<Set<string>>(new Set());
  const [working, setWorking] = useState<Record<string, string>>({});
  const menu = useContextMenu();

  const groups = useMemo(() => {
    const shown = (list ?? []).filter((c) => (!runningOnly || c.state === "running")
      && (!ctx.filter || [c.name, c.image, c.project ?? "", c.service ?? ""].some((s) => s.toLowerCase().includes(ctx.filter))));
    const by = new Map<string, DockerContainer[]>();
    for (const c of shown) by.set(c.project ?? "", [...(by.get(c.project ?? "") ?? []), c]);
    // Compose projects first (by name), then the containers on their own
    return [...by.entries()].sort(([a], [b]) => (a ? (b ? a.localeCompare(b) : -1) : 1)).map(([project, cs]) => ({ project, list: cs.sort((x, y) => (x.service ?? x.name).localeCompare(y.service ?? y.name)) }));
  }, [list, runningOnly, ctx.filter]);

  const act = async (c: DockerContainer, action: ContainerAction) => {
    if (action === "remove" && !(await confirmDialog({ title: `Remove ${c.name}?`, message: c.state === "running" ? "It's running: it's stopped first. Its volumes stay." : "Its volumes stay.", confirmLabel: "Remove", danger: true }))) return;
    if (action === "kill" && !(await confirmDialog({ title: `Kill ${c.name}?`, message: "It gets SIGKILL: no time to shut down cleanly.", confirmLabel: "Kill", danger: true }))) return;
    setWorking((w) => ({ ...w, [c.id]: action }));
    try {
      await docker.action(ctx.connId, c.id, action);
    } catch (e) {
      toast.error(`${c.name}: ${errMsg(e)}`);
    } finally {
      setWorking(({ [c.id]: _, ...w }) => w);
      ctx.refresh();
    }
  };
  const shell = async (c: DockerContainer) => {
    try {
      const { program, args } = await docker.shellCommand(ctx.connId, c.id);
      const home = await homeDir().catch(() => "/");
      openTab({ type: "terminal", cwd: home, program, args }, `Shell · ${c.name}`, `docker-shell:${ctx.connId}:${c.id}`);
    } catch (e) {
      toast.error(errMsg(e));
    }
  };
  const items = (c: DockerContainer): MenuItem[] => {
    const running = c.state === "running", paused = c.state === "paused";
    const change = (label: string, icon: ReactNode, action: ContainerAction): MenuItem => ({ label, icon, onClick: () => void act(c, action), disabled: ctx.readOnly });
    return [
      { label: "Logs", icon: <ScrollText className="size-3.5" />, onClick: () => ctx.show({ kind: "logs", id: c.id, name: c.name }) },
      ...(running ? [{ label: "Shell", icon: <SquareTerminal className="size-3.5" />, onClick: () => void shell(c), disabled: ctx.readOnly } as MenuItem] : []),
      { label: "Inspect", icon: <FileJson className="size-3.5" />, onClick: () => ctx.show({ kind: "inspect", obj: "container", id: c.id, name: c.name }) },
      "separator",
      ...(running ? [change("Stop", <CircleStop className="size-3.5" />, "stop"), change("Restart", <RotateCw className="size-3.5" />, "restart"), change("Pause", <Pause className="size-3.5" />, "pause"), change("Kill", <Skull className="size-3.5" />, "kill")]
        : paused ? [change("Resume", <Play className="size-3.5" />, "unpause"), change("Stop", <CircleStop className="size-3.5" />, "stop")]
        : [change("Start", <Play className="size-3.5" />, "start")]),
      "separator",
      change("Remove…", <Trash2 className="size-3.5" />, "remove"),
    ];
  };

  if (error && !list) return <ErrorBox error={error} onRetry={ctx.refresh} />;
  if (!list) return <Loading />;
  return (
    <div className="text-[12px]">
      <div className="sticky top-0 z-10">
        <div className="flex items-center gap-3 border-b border-line bg-panel px-3 py-1.5 text-[11px] text-muted">
          <label className="flex cursor-pointer items-center gap-1.5"><input type="checkbox" className="accent-[var(--color-accent)]" checked={runningOnly} onChange={(e) => setRunningOnly(e.target.checked)} />Running only</label>
          <span className="text-faint">{list.filter((c) => c.state === "running").length} of {list.length} running</span>
          {error && <span className="truncate text-danger" title={error}>· refresh failed</span>}
        </div>
        <div className={clsx(HEAD, CONTAINER_COLS)}>
          <span>Name</span><span>Image</span><span>Status</span><span>Ports</span><span className="text-right">CPU</span><span className="text-right">Memory</span><span />
        </div>
      </div>
      {!groups.length && <div className="px-3 py-6 text-center text-muted">{list.length ? "Nothing matches." : "No containers on this engine."}</div>}
      {groups.map(({ project, list: cs }) => {
        const open = !closed.has(project);
        const up = cs.filter((c) => c.state === "running").length;
        return (
          <div key={project || "-"}>
            <button className="flex w-full items-center gap-1.5 border-b border-line/60 bg-panel/60 px-2 py-1 text-left text-[11px] font-medium text-muted hover:text-fg"
              onClick={() => setClosed((s) => { const n = new Set(s); if (n.has(project)) n.delete(project); else n.add(project); return n; })}>
              <ChevronRight className={clsx("size-3 transition", open && "rotate-90")} />
              {project ? <><Layers className="size-3" />{project}</> : "Other containers"}
              <span className="font-normal text-faint">{up}/{cs.length} running</span>
            </button>
            {open && cs.map((c) => {
              const s = stats[c.id];
              const busy = working[c.id];
              const running = c.state === "running";
              return (
                <div key={c.id} onContextMenu={(e) => menu.show(e, items(c))} onDoubleClick={() => ctx.show({ kind: "logs", id: c.id, name: c.name })}
                  className={clsx("group grid items-center gap-2 border-b border-line/40 px-3 py-1.5 hover:bg-hover/50", CONTAINER_COLS)}>
                  <span className="flex min-w-0 items-center gap-2">
                    <span className={clsx("size-2 shrink-0 rounded-full", STATE_DOT[c.state] ?? "bg-faint")} title={c.state} />
                    <span className="min-w-0 truncate font-medium text-fg" title={c.name}>{c.service ?? c.name}</span>
                    {c.service && <span className="min-w-0 truncate text-[11px] text-faint" title={c.name}>{c.name}</span>}
                  </span>
                  <span className="min-w-0 truncate font-mono text-[11px] text-muted" title={c.image}>{c.image}</span>
                  <span className={clsx("min-w-0 truncate", running ? "text-fg/80" : "text-faint")} title={c.status}>{busy ? `${busy}…` : c.status}</span>
                  <span className="min-w-0 truncate font-mono text-[11px] text-muted" title={c.ports}>{c.ports}</span>
                  <span className="text-right font-mono text-[11px] text-muted" title="CPU">{s?.cpu ?? ""}</span>
                  <span className="truncate text-right font-mono text-[11px] text-muted" title={s ? `${s.memory} (${s.memoryPercent})` : "Memory"}>{s?.memory.split(" / ")[0] ?? ""}</span>
                  <span className="flex items-center justify-end gap-0.5">
                    {busy ? <Spinner className="size-3.5" /> : (
                      <>
                        <IconButton title="Logs" className="size-6 opacity-0 group-hover:opacity-100" onClick={() => ctx.show({ kind: "logs", id: c.id, name: c.name })}><ScrollText className="size-3.5" /></IconButton>
                        {running && <IconButton title="Shell" className="size-6 opacity-0 group-hover:opacity-100" disabled={ctx.readOnly} onClick={() => void shell(c)}><SquareTerminal className="size-3.5" /></IconButton>}
                        {running
                          ? <IconButton title="Stop" className="size-6 opacity-0 group-hover:opacity-100" disabled={ctx.readOnly} onClick={() => void act(c, "stop")}><CircleStop className="size-3.5" /></IconButton>
                          : <IconButton title="Start" className="size-6 opacity-0 group-hover:opacity-100" disabled={ctx.readOnly || c.state === "paused"} onClick={() => void act(c, "start")}><Play className="size-3.5" /></IconButton>}
                        <IconButton title="More" className="size-6" onClick={(e: MouseEvent) => menu.show(e, items(c))}><MoreHorizontal className="size-3.5" /></IconButton>
                      </>
                    )}
                  </span>
                </div>
              );
            })}
          </div>
        );
      })}
      {menu.element}
    </div>
  );
}

/** A plain list (images, volumes, networks) with Inspect and Remove. */
function Table<T>({ ctx, load, columns, rowKey, name, obj, removable = () => true, removeText }: {
  ctx: Ctx; load: () => Promise<T[]>; columns: { label: string; get: (r: T) => string; mono?: boolean; width: string }[];
  rowKey: (r: T) => string; name: (r: T) => string; obj: Exclude<DockerObject, "container">;
  removable?: (r: T) => boolean; removeText: string;
}) {
  const { list, error } = useList(load, [ctx.connId, ctx.tick]);
  const [working, setWorking] = useState<string | null>(null);
  const remove = async (r: T) => {
    const id = rowKey(r);
    if (!(await confirmDialog({ title: `Remove ${name(r)}?`, message: removeText, confirmLabel: "Remove", danger: true }))) return;
    setWorking(id);
    try {
      await docker.remove(ctx.connId, obj, id);
    } catch (e) {
      const msg = errMsg(e);
      // an image another tag or a stopped container still uses: say so, and offer to force it
      if (obj === "image" && /conflict|must be forced|being used|referenced in multiple/i.test(msg)
        && await confirmDialog({ title: "Remove it anyway?", message: msg, confirmLabel: "Force remove", danger: true })) {
        await docker.remove(ctx.connId, obj, id, true).catch((e2) => toast.error(errMsg(e2)));
      } else {
        toast.error(msg);
      }
    } finally {
      setWorking(null);
      ctx.refresh();
    }
  };
  if (error && !list) return <ErrorBox error={error} onRetry={ctx.refresh} />;
  if (!list) return <Loading />;
  const rows = list.filter((r) => !ctx.filter || columns.some((c) => c.get(r).toLowerCase().includes(ctx.filter)));
  const grid = { gridTemplateColumns: `${columns.map((c) => c.width).join(" ")} 56px` };
  return (
    <div className="text-[12px]">
      <div className={clsx("sticky top-0 z-10", HEAD)} style={grid}>
        {columns.map((c) => <span key={c.label}>{c.label}</span>)}<span />
      </div>
      {!rows.length && <div className="px-3 py-6 text-center text-muted">{list.length ? "Nothing matches." : `No ${obj}s.`}</div>}
      {rows.map((r) => (
        <div key={rowKey(r)} className="group grid items-center gap-2 border-b border-line/40 px-3 py-1.5 hover:bg-hover/50" style={grid}>
          {columns.map((c) => <span key={c.label} className={clsx("min-w-0 truncate", c.mono ? "font-mono text-[11px] text-muted" : "text-fg/90")} title={c.get(r)}>{c.get(r)}</span>)}
          <span className="flex items-center justify-end gap-0.5">
            {working === rowKey(r) ? <Spinner className="size-3.5" /> : (
              <>
                <IconButton title="Inspect" className="size-6 opacity-0 group-hover:opacity-100" onClick={() => ctx.show({ kind: "inspect", obj, id: rowKey(r), name: name(r) })}><FileJson className="size-3.5" /></IconButton>
                <IconButton title="Remove" className="size-6 opacity-0 group-hover:opacity-100" disabled={ctx.readOnly || !removable(r)} onClick={() => void remove(r)}><Trash2 className="size-3.5" /></IconButton>
              </>
            )}
          </span>
        </div>
      ))}
    </div>
  );
}

const short = (id: string) => id.replace(/^sha256:/, "").slice(0, 12);
/** A tagged image by its tag: removing it removes that tag only (an image can have several, each a
 *  row with the same ID). An untagged one by its ID. */
const imageRef = (r: DockerImage) => (r.repository === "<none>" || r.tag === "<none>" ? r.id : `${r.repository}:${r.tag}`);

function ImagesView({ ctx }: { ctx: Ctx }) {
  return (
    <Table ctx={ctx} obj="image" load={() => docker.images(ctx.connId)} rowKey={imageRef} removeText="Containers that use it keep running; it's pulled or built again when needed. If it has other tags, only this one goes."
      name={(r) => (r.repository === "<none>" ? short(r.id) : `${r.repository}:${r.tag}`)}
      columns={[
        { label: "Repository", get: (r) => r.repository, width: "minmax(180px,2fr)" },
        { label: "Tag", get: (r) => r.tag, width: "minmax(80px,1fr)" },
        { label: "ID", get: (r) => short(r.id), mono: true, width: "110px" },
        { label: "Size", get: (r) => r.size, width: "90px" },
        { label: "Created", get: (r) => r.created, width: "minmax(100px,1fr)" },
      ]} />
  );
}

function VolumesView({ ctx }: { ctx: Ctx }) {
  return (
    <Table ctx={ctx} obj="volume" load={() => docker.volumes(ctx.connId)} rowKey={(r) => r.name} name={(r) => r.name}
      removeText="Its data is deleted for good. A volume a container still uses can't be removed."
      columns={[
        { label: "Name", get: (r) => r.name, mono: true, width: "minmax(200px,2fr)" },
        { label: "Driver", get: (r) => r.driver, width: "90px" },
        { label: "Scope", get: (r) => r.scope, width: "70px" },
        { label: "Mountpoint", get: (r) => r.mountpoint, mono: true, width: "minmax(160px,2fr)" },
      ]} />
  );
}

function NetworksView({ ctx }: { ctx: Ctx }) {
  return (
    <Table ctx={ctx} obj="network" load={() => docker.networks(ctx.connId)} rowKey={(r) => r.id} name={(r) => r.name}
      removable={(r) => !["bridge", "host", "none"].includes(r.name)} removeText="Containers still connected to it keep it from being removed."
      columns={[
        { label: "Name", get: (r) => r.name, width: "minmax(180px,2fr)" },
        { label: "ID", get: (r) => short(r.id), mono: true, width: "110px" },
        { label: "Driver", get: (r) => r.driver, width: "90px" },
        { label: "Scope", get: (r) => r.scope, width: "70px" },
      ]} />
  );
}

const MAX_LINES = 5000;

/** A container's logs, following (`docker logs -f`): the last 500 lines, then new ones. A stop
 *  or restart ends the stream; if the container is running again within ~10 s (a restart), it
 *  follows again. */
function LogsPanel({ connId, id, name, onClose }: { connId: string; id: string; name: string; onClose: () => void }) {
  const lines = useRef<string[]>([]);
  const [, redraw] = useState(0);
  const [state, setState] = useState<{ done: boolean; error: string | null }>({ done: false, error: null });
  const [timestamps, setTimestamps] = useState(false);
  const [follow, setFollow] = useState(true);
  const [find, setFind] = useState("");
  const [round, setRound] = useState(0);
  const box = useRef<HTMLPreElement>(null);
  useEffect(() => {
    const streamId = `${id}:${Math.random().toString(36).slice(2)}`;
    let un: (() => void) | null = null;
    let live = true;
    let again: number | undefined;
    const resume = async (tries: number) => {
      const s = await docker.inspect(connId, "container", id).then((v) => (v as { State?: { Status?: string } }).State?.Status, () => null);
      if (!live || s === null) return;
      if (s === "running") setRound((r) => r + 1);
      else if (tries > 1) again = window.setTimeout(() => void resume(tries - 1), 2000);
    };
    lines.current = [];
    setState({ done: false, error: null });
    redraw((n) => n + 1);
    void listen<{ streamId: string; lines: string[]; done: boolean; error?: string | null }>("docker-log", (e) => {
      if (e.payload.streamId !== streamId) return;
      lines.current = [...lines.current, ...e.payload.lines].slice(-MAX_LINES);
      if (e.payload.done) {
        setState({ done: true, error: e.payload.error ?? null });
        if (!e.payload.error) again = window.setTimeout(() => void resume(5), 1500);
      }
      redraw((n) => n + 1);
    }).then((u) => {
      if (!live) return u();
      un = u;
      docker.logsStart(connId, id, streamId, 500, timestamps).catch((err) => setState({ done: true, error: errMsg(err) }));
    });
    return () => { live = false; window.clearTimeout(again); un?.(); void docker.logsStop(streamId); };
  }, [connId, id, timestamps, round]);
  useEffect(() => { if (follow && box.current) box.current.scrollTop = box.current.scrollHeight; });
  const q = find.trim().toLowerCase();
  const shown = q ? lines.current.filter((l) => l.toLowerCase().includes(q)) : lines.current;
  return (
    <>
      <PanelHead icon={<ScrollText className="size-3.5" />} title={`Logs · ${name}`} onClose={onClose}>
        {!state.done ? <span className="flex items-center gap-1 text-[11px] text-ok"><span className="size-1.5 animate-pulse rounded-full bg-ok" />following</span> : <button className="text-[11px] text-faint hover:text-fg" title="Follow again" onClick={() => setRound((r) => r + 1)}>ended · follow again</button>}
        <Input className="h-6 !w-44 text-xs" placeholder="Find in logs" value={find} onChange={(e) => setFind(e.target.value)} />
        <label className="flex items-center gap-1 text-[11px] text-muted"><input type="checkbox" className="accent-[var(--color-accent)]" checked={timestamps} onChange={(e) => setTimestamps(e.target.checked)} />Times</label>
        <label className="flex items-center gap-1 text-[11px] text-muted"><input type="checkbox" className="accent-[var(--color-accent)]" checked={follow} onChange={(e) => setFollow(e.target.checked)} />Follow</label>
        <IconButton title="Copy" onClick={() => void navigator.clipboard.writeText(shown.join("\n")).then(() => toast.success("Copied"))}><Copy className="size-3.5" /></IconButton>
      </PanelHead>
      {state.error && <div className="px-3 py-1 text-[11px] text-danger">{state.error}</div>}
      <pre ref={box} className="selectable min-h-0 flex-1 overflow-auto bg-panel px-3 py-2 font-mono text-[11.5px] leading-relaxed whitespace-pre-wrap text-fg/90">
        {shown.length ? shown.join("\n") : <span className="text-faint">{state.done ? "No log lines." : "Waiting for lines…"}</span>}
      </pre>
    </>
  );
}

function InspectPanel({ connId, obj, id, name, onClose }: { connId: string; obj: DockerObject; id: string; name: string; onClose: () => void }) {
  const [json, setJson] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    docker.inspect(connId, obj, id).then((v) => live && setJson(JSON.stringify(v, null, 2)), (e) => live && setError(errMsg(e)));
    return () => { live = false; };
  }, [connId, obj, id]);
  return (
    <>
      <PanelHead icon={<FileJson className="size-3.5" />} title={`Inspect · ${name}`} onClose={onClose}>
        {json && <IconButton title="Copy" onClick={() => void navigator.clipboard.writeText(json).then(() => toast.success("Copied"))}><Copy className="size-3.5" /></IconButton>}
      </PanelHead>
      {error ? <div className="px-3 py-2 text-[12px] text-danger">{error}</div>
        : json === null ? <Loading />
        : <pre className="selectable min-h-0 flex-1 overflow-auto bg-panel px-3 py-2 font-mono text-[11.5px] leading-relaxed text-fg/90">{json}</pre>}
    </>
  );
}

function PanelHead({ icon, title, onClose, children }: { icon: ReactNode; title: string; onClose: () => void; children?: ReactNode }) {
  return (
    <div className="flex h-8 shrink-0 items-center gap-2 border-b border-line bg-panel px-2 text-[12px]">
      <span className="text-muted">{icon}</span>
      <span className="min-w-0 truncate font-medium text-fg">{title}</span>
      <div className="flex-1" />
      {children}
      <IconButton title="Close" onClick={onClose}><X className="size-3.5" /></IconButton>
    </div>
  );
}

function Loading() {
  return <div className="flex justify-center py-8"><Spinner className="size-5" /></div>;
}

function ErrorBox({ error, onRetry }: { error: string; onRetry: () => void }) {
  return (
    <div className="m-3 flex items-start gap-2 rounded-md border border-danger/30 bg-danger/10 px-3 py-2 text-[12px] text-danger">
      <span className="min-w-0 flex-1 whitespace-pre-wrap">{error}</span>
      <Button size="sm" onClick={onRetry}>Retry</Button>
    </div>
  );
}
