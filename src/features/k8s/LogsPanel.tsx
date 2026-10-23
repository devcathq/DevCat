import { save } from "@tauri-apps/plugin-dialog";
import { useVirtualizer } from "@tanstack/react-virtual";
import clsx from "clsx";
import { ArrowDownToLine, ChevronDown, ChevronUp, Download, Eraser, WrapText } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import { IconButton, Input, Select } from "../../components/ui";
import { errMsg } from "../../lib/api";
import { events, eventsReady, k8s } from "./api";
import PodSwitcher from "./PodSwitcher";

const MAX_LINES = 50_000;
const ALL = "__all__";

export interface LogTarget { namespace: string; pod: string; containers: string[]; initContainers: string[]; container?: string | null; owner?: string; pods?: string[] }

function Toggle({ on, onClick, children, title }: { on: boolean; onClick: () => void; children: React.ReactNode; title?: string }) {
  return (
    <button
      title={title}
      onClick={onClick}
      className={clsx("h-6 rounded px-2 text-xs border", on ? "border-accent/50 bg-accent/15 text-accent" : "border-line text-muted hover:text-fg hover:bg-hover")}
    >
      {children}
    </button>
  );
}

function highlight(line: string, q: string, active: boolean) {
  if (!q) return line;
  const lower = line.toLowerCase();
  const out: React.ReactNode[] = [];
  let i = 0, k = 0;
  for (;;) {
    const j = lower.indexOf(q, i);
    if (j < 0) break;
    if (j > i) out.push(line.slice(i, j));
    out.push(<mark key={k++} className={clsx("solid rounded-sm text-app", active ? "bg-accent" : "bg-warn/70")}>{line.slice(j, j + q.length)}</mark>);
    i = j + q.length;
  }
  out.push(line.slice(i));
  return out;
}

export default function LogsPanel({ connId, target, visible, onPodChange }: { connId: string; target: LogTarget; visible: boolean; onPodChange?: (pod: string) => void }) {
  const [container, setContainer] = useState<string>(target.container ?? target.containers[0] ?? "");
  const [follow, setFollow] = useState(true);
  const [timestamps, setTimestamps] = useState(false);
  const [previous, setPrevious] = useState(false);
  const [tail, setTail] = useState("1000");
  const [wrap, setWrap] = useState(false);
  const [query, setQuery] = useState("");
  const [matchIdx, setMatchIdx] = useState(0);
  const [status, setStatus] = useState<{ state: "loading" | "streaming" | "ended" | "error"; msg?: string }>({ state: "loading" });

  const lines = useRef<string[]>([]);
  const [version, setVersion] = useState(0);
  const stick = useRef(true);
  const scrollRef = useRef<HTMLDivElement>(null);

  const containersKey = target.containers.join("\n");
  // (Re)start streams on option change.
  useEffect(() => {
    let cancelled = false;
    const ids: string[] = [];
    const offs: (() => void)[] = [];
    lines.current = [];
    setVersion((v) => v + 1);
    setStatus({ state: "loading" });
    stick.current = true;
    const containers = container === ALL ? containersKey.split("\n") : [container];
    let ended = 0;
    (async () => {
      await eventsReady();
      for (const c of containers) {
        const prefix = container === ALL ? `[${c}] ` : "";
        try {
          const id = await k8s.logsStart(connId, target.namespace, target.pod, {
            container: c, follow, previous, timestamps, tailLines: tail === "all" ? null : Number(tail),
          });
          if (cancelled) { k8s.streamStop(id).catch(() => {}); return; }
          ids.push(id);
          offs.push(events.log.on(id, (b) => {
            const buf = lines.current;
            for (const l of b.lines) buf.push(prefix + l);
            if (buf.length > MAX_LINES) buf.splice(0, buf.length - MAX_LINES);
            setVersion((v) => v + 1);
          }));
          offs.push(events.logEnd.on(id, (e) => {
            ended++;
            if (e.error) setStatus({ state: "error", msg: e.error });
            else if (ended === containers.length) setStatus({ state: "ended" });
          }));
          if (!cancelled) setStatus((s) => (s.state === "loading" ? { state: follow ? "streaming" : "loading" } : s));
        } catch (e) {
          if (!cancelled) setStatus({ state: "error", msg: errMsg(e) });
          return;
        }
      }
    })();
    return () => {
      cancelled = true;
      offs.forEach((f) => f());
      ids.forEach((id) => k8s.streamStop(id).catch(() => {}));
    };
  }, [connId, target.namespace, target.pod, container, follow, previous, timestamps, tail, containersKey]);

  const q = query.trim().toLowerCase();
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const all = useMemo(() => lines.current.slice(), [version]);
  const matches = useMemo(() => {
    if (!q) return [] as number[];
    const m: number[] = [];
    all.forEach((l, i) => { if (l.toLowerCase().includes(q)) m.push(i); });
    return m;
  }, [all, q]);

  const virt = useVirtualizer({
    count: all.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 18,
    overscan: 30,
  });

  useEffect(() => { virt.measure(); }, [wrap, virt]);

  // Follow tail.
  useEffect(() => {
    if (stick.current && all.length && visible && !q) virt.scrollToIndex(all.length - 1, { align: "end" });
  }, [all.length, virt, visible, q]);

  const gotoMatch = useCallback((i: number) => {
    if (!matches.length) return;
    const n = (i + matches.length) % matches.length;
    setMatchIdx(n);
    stick.current = false;
    virt.scrollToIndex(matches[n], { align: "center" });
  }, [matches, virt]);

  useEffect(() => { setMatchIdx(0); if (matches.length) gotoMatch(0); /* jump to first match on new query */ // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [q]);

  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
  };

  const download = async () => {
    try {
      const path = await save({ defaultPath: `${target.pod}${container && container !== ALL ? `-${container}` : ""}.log` });
      if (!path) return;
      await k8s.saveText(path, lines.current.join("\n") + "\n");
      toast.success(`Saved ${lines.current.length} lines`);
    } catch (e) {
      toast.error(errMsg(e));
    }
  };

  const activeLine = matches.length ? matches[matchIdx] : -1;

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex h-8 shrink-0 items-center gap-1.5 border-b border-line px-2">
        {target.pods && target.pods.length > 0 && onPodChange && (
          <PodSwitcher owner={target.owner} pods={target.pods} pod={target.pod} onChange={onPodChange} />
        )}
        <Select className="h-6 w-auto max-w-48 text-xs" value={container} onChange={(e) => setContainer(e.target.value)}>
          {target.containers.length > 1 && <option value={ALL}>All containers</option>}
          {target.containers.map((c) => <option key={c} value={c}>{c}</option>)}
          {target.initContainers.map((c) => <option key={`init-${c}`} value={c}>{c} (init)</option>)}
        </Select>
        <Select className="h-6 w-auto text-xs" value={tail} onChange={(e) => setTail(e.target.value)} title="Lines of history">
          {["100", "500", "1000", "5000", "all"].map((t) => <option key={t} value={t}>{t === "all" ? "All lines" : `Last ${t}`}</option>)}
        </Select>
        <Toggle on={follow} onClick={() => setFollow(!follow)} title="Stream new lines">Follow</Toggle>
        <Toggle on={timestamps} onClick={() => setTimestamps(!timestamps)}>Timestamps</Toggle>
        <Toggle on={previous} onClick={() => setPrevious(!previous)} title="Logs of the previous (crashed) container">Previous</Toggle>
        <div className="mx-1 h-4 w-px bg-line" />
        <div className="relative w-56">
          <Input
            className="h-6 pr-16 text-xs"
            placeholder="Search…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter") gotoMatch(matchIdx + (e.shiftKey ? -1 : 1)); if (e.key === "Escape") setQuery(""); }}
          />
          {q && <span className="absolute right-2 top-1 text-[11px] text-muted tabular-nums">{matches.length ? `${matchIdx + 1}/${matches.length}` : "0/0"}</span>}
        </div>
        <IconButton title="Previous match (Shift+Enter)" onClick={() => gotoMatch(matchIdx - 1)} disabled={!matches.length}><ChevronUp className="size-3.5" /></IconButton>
        <IconButton title="Next match (Enter)" onClick={() => gotoMatch(matchIdx + 1)} disabled={!matches.length}><ChevronDown className="size-3.5" /></IconButton>
        <div className="flex-1" />
        <span className={clsx("text-[11px]", status.state === "error" ? "text-danger" : "text-muted")} title={status.msg}>
          {status.state === "streaming" ? `● live · ${all.length.toLocaleString()} lines` : status.state === "ended" ? `${all.length.toLocaleString()} lines · ended` : status.state === "error" ? "error" : `${all.length.toLocaleString()} lines`}
          {all.length >= MAX_LINES && " (capped)"}
        </span>
        <IconButton title="Wrap lines" active={wrap} onClick={() => setWrap(!wrap)}><WrapText className="size-3.5" /></IconButton>
        <IconButton title="Scroll to bottom" onClick={() => { stick.current = true; virt.scrollToIndex(all.length - 1, { align: "end" }); }}><ArrowDownToLine className="size-3.5" /></IconButton>
        <IconButton title="Clear" onClick={() => { lines.current = []; setVersion((v) => v + 1); }}><Eraser className="size-3.5" /></IconButton>
        <IconButton title="Download" onClick={download}><Download className="size-3.5" /></IconButton>
      </div>
      {status.state === "error" && (
        <div className="selectable shrink-0 border-b border-danger/30 bg-danger/10 px-3 py-1.5 font-mono text-xs text-danger">{status.msg}</div>
      )}
      <div ref={scrollRef} onScroll={onScroll} className="selectable min-h-0 flex-1 overflow-auto bg-app font-mono text-[12px] leading-[18px]">
        {all.length === 0 && status.state !== "error" && (
          <div className="p-3 text-xs text-muted">{status.state === "loading" ? "Loading logs…" : "No log output."}</div>
        )}
        <div style={{ height: virt.getTotalSize(), position: "relative", width: wrap ? "100%" : "max-content", minWidth: "100%" }}>
          {virt.getVirtualItems().map((vr) => {
            const l = all[vr.index];
            return (
              <div
                key={vr.key}
                data-index={vr.index}
                ref={wrap ? virt.measureElement : undefined}
                className={clsx("absolute left-0 w-full px-3", wrap ? "whitespace-pre-wrap break-all" : "whitespace-pre", vr.index === activeLine && "bg-accent/10")}
                style={{ top: vr.start, height: wrap ? undefined : 18 }}
              >
                {highlight(l, q, vr.index === activeLine)}
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}
