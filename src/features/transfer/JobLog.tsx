// Progress + log area shared by every Data Transfer sub-tool.
import clsx from "clsx";
import { CheckCircle2, CircleAlert, Info, Loader2, Trash2, XCircle } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { IconButton, Spin } from "../../components/ui";
import { newJobId, useTransferProgress, type TransferProgress } from "./shared";

export type LogTone = "info" | "ok" | "warn" | "error";
export interface LogLine { at: Date; tone: LogTone; text: string }

export interface JobState {
  jobId: string | null;
  running: boolean;
  startedAt?: number;
  finishedAt?: number;
  /** final summary */
  result?: { ok: boolean; text: string };
  /** current step label set by the caller (e.g. table being exported) */
  step?: string;
}

export function useJob() {
  const [job, setJob] = useState<JobState>({ jobId: null, running: false });
  const [log, setLog] = useState<LogLine[]>([]);
  const lastTable = useRef<string | null>(null);
  const append = useCallback((text: string, tone: LogTone = "info") => setLog((l) => [...l.slice(-999), { at: new Date(), tone, text }]), []);
  const progress = useTransferProgress(job.jobId, (p: TransferProgress) => {
    if (p.table && p.table !== lastTable.current) {
      lastTable.current = p.table;
      append(`Processing ${p.table}${p.tables ? ` (${(p.tableIndex ?? 0) + 1}/${p.tables})` : ""}…`);
    }
    if (p.error) append(p.error, "error");
  });
  const start = useCallback((title: string) => {
    const jobId = newJobId();
    lastTable.current = null;
    setJob({ jobId, running: true, startedAt: Date.now() });
    append(title);
    return jobId;
  }, [append]);
  const finish = useCallback((ok: boolean, text: string) => {
    setJob((j) => ({ ...j, running: false, finishedAt: Date.now(), result: { ok, text } }));
    append(text, ok ? "ok" : "error");
  }, [append]);
  const clear = useCallback(() => { setLog([]); setJob({ jobId: null, running: false }); }, []);
  const setStep = useCallback((step: string) => setJob((j) => ({ ...j, step })), []);
  return { job, log, progress, start, finish, append, clear, setStep };
}

export type Job = ReturnType<typeof useJob>;

export function JobLog({ job }: { job: Job }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => { ref.current?.scrollTo({ top: ref.current.scrollHeight }); }, [job.log.length]);
  const { running, startedAt, finishedAt, result } = job.job;
  const [, tick] = useState(0);
  useEffect(() => {
    if (!running) return;
    const t = setInterval(() => tick((n) => n + 1), 500);
    return () => clearInterval(t);
  }, [running]);
  const elapsed = startedAt ? ((finishedAt ?? Date.now()) - startedAt) / 1000 : 0;
  const p = job.progress;
  const rows = p?.rows ?? 0;
  const rate = elapsed > 0.5 && rows ? Math.round(rows / elapsed) : 0;
  // Determinate progress when the backend reports totals.
  const frac = p?.tables && p.tables > 0
    ? ((p.tableIndex ?? 0) + (p.tableTotal ? Math.min(1, (p.tableRows ?? 0) / p.tableTotal) : 0)) / p.tables
    : p?.total ? Math.min(1, rows / p.total) : null;
  const label = p?.table ?? job.job.step;

  return (
    <div className="flex h-full flex-col bg-panel">
      <div className="flex h-8 shrink-0 items-center gap-2 border-b border-line px-3 text-xs">
        <span className="text-[11px] font-semibold uppercase tracking-wider text-muted">Progress</span>
        {running && <Spin icon={Loader2} className="size-3.5 text-accent" />}
        {!running && result && (result.ok ? <CheckCircle2 className="size-3.5 text-ok" /> : <XCircle className="size-3.5 text-danger" />)}
        {(running || result) && (
          <span className="text-muted">
            {rows.toLocaleString()}{p?.total ? ` / ${p.total.toLocaleString()}` : ""} rows
            {p?.tables ? ` · table ${(p.tableIndex ?? 0) + 1}/${p.tables}` : ""}
            {label ? ` · ${label}` : ""} · {elapsed.toFixed(1)}s{rate ? ` · ${rate.toLocaleString()} rows/s` : ""}
            {p?.failed ? <span className="text-danger"> · {p.failed} failed</span> : null}
          </span>
        )}
        <div className="flex-1" />
        <IconButton title="Clear log" onClick={job.clear} disabled={running}><Trash2 className="size-3.5" /></IconButton>
      </div>
      <div className="relative h-0.5 shrink-0 overflow-hidden bg-line">
        {running && frac !== null && <div className="absolute inset-y-0 left-0 bg-accent transition-[width]" style={{ width: `${Math.round(frac * 100)}%` }} />}
        {running && frac === null && <div className="absolute inset-y-0 left-0 w-1/3 animate-[devcat-indeterminate_1.2s_ease-in-out_infinite] bg-accent" />}
        {!running && result && <div className={clsx("absolute inset-0", result.ok ? "bg-ok" : "bg-danger")} />}
      </div>
      <div ref={ref} className="selectable min-h-0 flex-1 overflow-auto px-3 py-1.5 font-mono text-[11.5px] leading-5">
        {!job.log.length && <div className="text-faint">No activity yet.</div>}
        {job.log.map((l, i) => (
          <div key={i} className="flex gap-2">
            <span className="shrink-0 text-faint">{l.at.toLocaleTimeString()}</span>
            <LogIcon tone={l.tone} />
            <span className={clsx("whitespace-pre-wrap break-all", l.tone === "error" ? "text-danger" : l.tone === "ok" ? "text-ok" : l.tone === "warn" ? "text-warn" : "text-fg/85")}>{l.text}</span>
          </div>
        ))}
      </div>
      {/* Moved by transform (in steps of the bar's own width), not `left`: animating `left` lays
          the page out again on every frame. */}
      <style>{`@keyframes devcat-indeterminate { 0% { transform: translateX(-100%) } 100% { transform: translateX(300%) } }`}</style>
    </div>
  );
}

function LogIcon({ tone }: { tone: LogTone }) {
  const c = "mt-[3px] size-3.5 shrink-0";
  if (tone === "ok") return <CheckCircle2 className={clsx(c, "text-ok")} />;
  if (tone === "error") return <XCircle className={clsx(c, "text-danger")} />;
  if (tone === "warn") return <CircleAlert className={clsx(c, "text-warn")} />;
  return <Info className={clsx(c, "text-faint")} />;
}
