// The Trace view of a DevCat Agent chat: every turn as it happened, from the session log. Each model
// step with its time and tokens, each tool call with its input, output and time, approvals, hooks,
// summaries, and the helpers' own runs nested under their calls. The model is in ./trace.ts.
import clsx from "clsx";
import { Ban, Check, ChevronRight, CircleAlert, Cpu, FileText, FoldVertical, MessageSquare, RefreshCw, Webhook, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
import { Button, Empty, Spinner } from "../../components/ui";
import { api, errMsg } from "../../lib/api";
import type { AgentTrace, AgentUsage } from "../../lib/types";
import { openFile } from "../claude/util";
import { describe, fmtDuration, fmtTokens } from "./model";
import { useAgent } from "./store";
import { buildTrace, helperFile, type TraceCall, type TraceRow, type TraceTurn } from "./trace";

export default function TraceView({ tabId, cwd, active }: { tabId: string; cwd: string; active: boolean }) {
  const sessionId = useAgent((s) => s.chats[tabId]?.sessionId ?? null);
  const busy = useAgent((s) => !!s.chats[tabId]?.busy);
  const [trace, setTrace] = useState<AgentTrace | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const load = useCallback(async () => {
    if (!sessionId) return;
    setLoading(true);
    try {
      setTrace(await api.agentTrace(sessionId));
      setError(null);
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setLoading(false);
    }
  }, [sessionId]);
  // when shown, when a turn ends, and every few seconds while one runs
  useEffect(() => { if (active) void load(); }, [active, busy, load]);
  useEffect(() => {
    if (!active || !busy) return;
    const t = setInterval(() => void load(), 4000);
    return () => clearInterval(t);
  }, [active, busy, load]);
  const model = useMemo(() => (trace ? buildTrace(trace.events) : null), [trace]);

  if (!sessionId || (error && !trace)) return <Empty icon={<FoldVertical className="size-8" />} title="No trace yet">{error ?? "Send a message: every step of the chat shows up here."}</Empty>;
  if (!model || !trace) return <div className="flex h-full items-center justify-center"><Spinner className="size-5" /></div>;
  const helperCalls = Object.keys(trace.helpers).length;
  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex shrink-0 flex-wrap items-center gap-x-4 gap-y-1 border-b border-line px-5 py-2 text-[12px] text-muted">
        <span className="text-fg">{model.turns.length} turn{model.turns.length === 1 ? "" : "s"} · {model.steps} model step{model.steps === 1 ? "" : "s"} · {model.calls} tool call{model.calls === 1 ? "" : "s"}{helperCalls ? ` (${helperCalls} helper${helperCalls === 1 ? "" : "s"})` : ""}</span>
        <Tokens usage={model.usage} />
        {model.busyMs !== null && <span title="Time spent working, from each message to the end of its turn">{fmtDuration(model.busyMs)} working</span>}
        {model.models.length > 0 && <span className="flex items-center gap-1"><Cpu className="size-3" />{model.models.join(", ")}</span>}
        <div className="flex-1" />
        <Button size="sm" variant="ghost" title="Open the session log (JSONL)" onClick={() => openFile(cwd, trace.path)}><FileText className="size-3.5" />Log</Button>
        <Button size="sm" variant="ghost" loading={loading} onClick={() => void load()}><RefreshCw className="size-3.5" />Refresh</Button>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto flex max-w-[980px] flex-col gap-4 px-5 py-4">
          {model.turns.length === 0 && <p className="text-[12px] text-muted">Nothing in this chat yet.</p>}
          {model.turns.map((t, i) => <Turn key={i} turn={t} n={i + 1} trace={trace} cwd={cwd} />)}
          {model.busyMs === null && model.turns.length > 0 && <p className="text-[11px] text-faint">This chat was saved by an older DevCat: its steps have no times.</p>}
        </div>
      </div>
    </div>
  );
}

function Tokens({ usage }: { usage: AgentUsage }) {
  const cached = Math.min(usage.cache_read_tokens, usage.input_tokens);
  return (
    <span title={`${usage.input_tokens.toLocaleString()} input tokens (${cached.toLocaleString()} from the cache), ${usage.output_tokens.toLocaleString()} output tokens`}>
      {fmtTokens(usage.input_tokens)} in{cached ? ` (${Math.round((cached / Math.max(1, usage.input_tokens)) * 100)}% cached)` : ""} · {fmtTokens(usage.output_tokens)} out
    </span>
  );
}

/** Where a span sits in its turn: a bar along the turn's duration. */
function Bar({ turn, start, took, tone = "accent" }: { turn: TraceTurn; start: number | null; took: number | null; tone?: "accent" | "ok" | "danger" | "muted" }) {
  const total = turn.startMs !== null && turn.endMs !== null ? Math.max(1, turn.endMs - turn.startMs) : null;
  if (total === null || start === null || took === null) return <span className="w-28 shrink-0" />;
  const left = Math.min(100, Math.max(0, ((start - turn.startMs!) / total) * 100));
  const width = Math.max(1.5, Math.min(100 - left, (took / total) * 100));
  return (
    <span className="relative h-1.5 w-28 shrink-0 self-center rounded-full bg-line/50" title={`${fmtDuration(took)}, from +${fmtDuration(start - turn.startMs!)}`}>
      <span className={clsx("absolute inset-y-0 rounded-full", tone === "accent" ? "bg-accent/80" : tone === "ok" ? "bg-ok/70" : tone === "danger" ? "bg-danger/70" : "bg-faint")} style={{ left: `${left}%`, width: `${width}%` }} />
    </span>
  );
}

const offset = (turn: TraceTurn, ms: number | null) => (turn.startMs !== null && ms !== null ? `+${((ms - turn.startMs) / 1000).toFixed(1)}s` : "");

function Turn({ turn, n, trace, cwd }: { turn: TraceTurn; n: number; trace: AgentTrace; cwd: string }) {
  const first = turn.rows[0];
  const title = first?.kind === "user" && !first.hook ? first.text.split("\n")[0] : "Continued";
  return (
    <section className="rounded-lg border border-line bg-panel/40">
      <header className="flex items-center gap-3 border-b border-line px-3 py-2 text-[12px]">
        <span className="shrink-0 font-semibold text-fg">Turn {n}</span>
        <span className="min-w-0 flex-1 truncate text-muted" title={title}>{title}</span>
        {turn.startMs !== null && turn.endMs !== null && <span className="shrink-0 text-faint">{fmtDuration(turn.endMs - turn.startMs)}</span>}
        <span className="shrink-0 text-faint"><Tokens usage={turn.usage} /></span>
      </header>
      <div className="flex flex-col py-1">
        {turn.rows.map((r, i) => <Row key={i} row={r} turn={turn} trace={trace} cwd={cwd} />)}
      </div>
    </section>
  );
}

function Line({ time, bar, icon, children, onClick, open }: { time: string; bar: ReactNode; icon: ReactNode; children: ReactNode; onClick?: () => void; open?: boolean }) {
  return (
    <div onClick={onClick} className={clsx("flex min-w-0 items-start gap-2 px-3 py-1 text-[12px]", onClick && "cursor-pointer hover:bg-hover/50")}>
      <span className="w-12 shrink-0 text-right font-mono text-[10.5px] leading-5 text-faint">{time}</span>
      {bar}
      <span className="mt-[3px] shrink-0 text-muted">{icon}</span>
      <div className="min-w-0 flex-1 leading-5">{children}</div>
      {onClick && <ChevronRight className={clsx("mt-1 size-3 shrink-0 text-faint transition", open && "rotate-90")} />}
    </div>
  );
}

function Row({ row, turn, trace, cwd }: { row: TraceRow; turn: TraceTurn; trace: AgentTrace; cwd: string }) {
  const [open, setOpen] = useState(false);
  switch (row.kind) {
    case "user":
      return (
        <>
          <Line time={offset(turn, row.ms)} bar={<span className="w-28 shrink-0" />} icon={row.hook ? <Webhook className="size-3.5" /> : <MessageSquare className="size-3.5" />}
            onClick={row.context.length || row.text.length > 160 ? () => setOpen(!open) : undefined} open={open}>
            <span className="text-fg">{row.hook ? `${row.hook} hook` : "You"}</span>
            <span className="text-muted"> · {open ? "" : row.text.replace(/\s+/g, " ").slice(0, 160)}</span>
            {row.context.length > 0 && <span className="ml-1 text-[11px] text-faint">+ hook context</span>}
          </Line>
          {open && <Details><Pre>{row.text}</Pre>{row.context.map((c, i) => <Pre key={i} muted>{`Hook context:\n${c}`}</Pre>)}</Details>}
        </>
      );
    case "step":
      return (
        <>
          <Line time={offset(turn, row.startMs)} bar={<Bar turn={turn} start={row.startMs} took={row.tookMs} />} icon={<Cpu className="size-3.5" />}
            onClick={row.text || row.thinking ? () => setOpen(!open) : undefined} open={open}>
            <span className="text-fg">Model</span>
            <span className="text-muted"> · {row.tookMs !== null ? `${fmtDuration(row.tookMs)} · ` : ""}<Tokens usage={row.usage} />{row.thinking ? " · thought" : ""}</span>
            {!open && row.text.trim() && <div className="truncate text-muted">{row.text.replace(/\s+/g, " ")}</div>}
          </Line>
          {open && <Details>{row.thinking && <Pre muted>{row.thinking}</Pre>}{row.text && <Pre>{row.text}</Pre>}</Details>}
          {row.calls.map((c) => <Call key={c.id} call={c} turn={turn} trace={trace} cwd={cwd} />)}
        </>
      );
    case "compacted":
      return (
        <>
          <Line time={offset(turn, row.startMs)} bar={<Bar turn={turn} start={row.startMs} took={row.tookMs} tone="muted" />} icon={<FoldVertical className="size-3.5" />} onClick={() => setOpen(!open)} open={open}>
            <span className="text-fg">Summarized the conversation</span>
            <span className="text-muted"> · {fmtTokens(row.tokensBefore)} tokens before · <Tokens usage={row.usage} /></span>
          </Line>
          {open && <Details><Pre muted>{row.summary}</Pre></Details>}
        </>
      );
    case "interrupted":
      return (
        <Line time={offset(turn, row.ms)} bar={<span className="w-28 shrink-0" />} icon={<CircleAlert className="size-3.5 text-warn" />}>
          <span className="text-warn">{row.reason}</span>
        </Line>
      );
  }
}

function Call({ call, turn, trace, cwd }: { call: TraceCall; turn: TraceTurn; trace: AgentTrace; cwd: string }) {
  const [open, setOpen] = useState(false);
  const status = call.output === null ? "pending" : call.isError ? "error" : "done";
  const d = describe(call.name, call.input, status === "pending" ? "running" : status, cwd);
  const Icon = d.icon;
  const helper = call.name === "subagent" ? trace.helpers[helperFile(call.id)] : undefined;
  const nested = useMemo(() => (helper ? buildTrace(helper) : null), [helper]);
  // the part of its time spent waiting for the answer
  const waited = call.approval?.ms != null && call.startMs !== null ? Math.max(0, call.approval.ms - call.startMs) : null;
  return (
    <>
      <Line time={offset(turn, call.startMs)} bar={<Bar turn={turn} start={call.startMs} took={call.tookMs} tone={call.isError ? "danger" : "ok"} />} icon={<span className="ml-3 inline-flex"><Icon className="size-3.5" /></span>} onClick={() => setOpen(!open)} open={open}>
        <span className="text-fg">{d.verb}</span> <span className="font-mono text-[11.5px] text-muted">{d.target}</span>
        <span className="ml-2 inline-flex items-center gap-1 text-[11px] text-faint">
          {call.tookMs !== null && fmtDuration(call.tookMs)}
          {call.approval && <span className={call.approval.allowed ? "text-ok" : "text-danger"}>{call.approval.allowed ? "allowed" : "declined"} by {call.approval.by}{waited !== null && waited >= 1000 ? ` after ${fmtDuration(waited)}` : ""}</span>}
          {status === "done" ? <Check className="size-3 text-ok" /> : status === "error" ? (call.approval?.allowed === false ? <Ban className="size-3" /> : <X className="size-3 text-danger" />) : null}
        </span>
      </Line>
      {open && (
        <Details>
          <Pre muted>{JSON.stringify(call.input, null, 2)}</Pre>
          {call.output !== null && <Pre danger={call.isError}>{call.output}</Pre>}
          {nested && (
            <div className="flex flex-col gap-2 border-l-2 border-accent/30 pl-3">
              <span className="text-[11px] text-faint">The helper's run: {nested.steps} model steps · {nested.calls} tool calls · <Tokens usage={nested.usage} /></span>
              {nested.turns.map((t, i) => <Turn key={i} turn={t} n={i + 1} trace={{ ...trace, helpers: {} }} cwd={cwd} />)}
            </div>
          )}
        </Details>
      )}
    </>
  );
}

function Details({ children }: { children: ReactNode }) {
  return <div className="ml-[13.5rem] mr-3 mb-1.5 flex flex-col gap-1.5">{children}</div>;
}

function Pre({ children, muted, danger }: { children: ReactNode; muted?: boolean; danger?: boolean }) {
  return <pre className={clsx("max-h-80 overflow-auto rounded-md border border-line bg-panel px-2.5 py-1.5 font-mono text-[11px] leading-relaxed whitespace-pre-wrap", danger ? "text-danger" : muted ? "text-muted" : "text-fg")}>{children}</pre>;
}
