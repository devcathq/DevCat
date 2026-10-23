// The Trace view's model: a chat's log (crates/harness/src/session.rs, each line with `ms`, when it
// was written) turned into turns of model steps and tool calls, with timings and tokens. Logs from
// before `ms` was written have no timings; everything else still shows.
import type { AgentTraceEvent, AgentUsage } from "../../lib/types";
import { ZERO } from "./model";

export interface TraceCall {
  id: string;
  name: string;
  input: Record<string, unknown> | null;
  output: string | null;
  isError: boolean;
  /** When it started and how long it took (ms), when known. */
  startMs: number | null;
  tookMs: number | null;
  /** The approval it needed: who answered, how, and when (ms). */
  approval: { allowed: boolean; by: string; ms: number | null } | null;
}

export type TraceRow =
  | { kind: "user"; ms: number | null; text: string; context: string[]; hook: string | null }
  | { kind: "step"; ms: number | null; startMs: number | null; tookMs: number | null; model: string; usage: AgentUsage; text: string; thinking: string; calls: TraceCall[] }
  | { kind: "compacted"; ms: number | null; startMs: number | null; tookMs: number | null; usage: AgentUsage; tokensBefore: number; summary: string }
  | { kind: "interrupted"; ms: number | null; reason: string };

export interface TraceTurn {
  rows: TraceRow[];
  startMs: number | null;
  endMs: number | null;
  /** Tokens of its steps and summaries, plus what its helpers used. */
  usage: AgentUsage;
}

export interface Trace {
  turns: TraceTurn[];
  usage: AgentUsage;
  steps: number;
  calls: number;
  models: string[];
  /** Time spent in turns (not between them), when known. */
  busyMs: number | null;
}

const plus = (a: AgentUsage, b: AgentUsage | undefined): AgentUsage => (b ? { input_tokens: a.input_tokens + b.input_tokens, output_tokens: a.output_tokens + b.output_tokens, cache_read_tokens: a.cache_read_tokens + b.cache_read_tokens } : a);

/** A helper's log file name for its `subagent` call (as the harness names it: `file_safe`). */
export const helperFile = (callId: string) => callId.replace(/[^A-Za-z0-9_-]/g, "_");

export function buildTrace(events: AgentTraceEvent[]): Trace {
  const turns: TraceTurn[] = [];
  let turn: TraceTurn | null = null;
  let prevMs: number | null = null;
  const calls = new Map<string, TraceCall>();
  let lastStep: Extract<TraceRow, { kind: "step" }> | null = null;
  const models = new Set<string>();
  const current = (ms: number | null) => {
    if (!turn) {
      turn = { rows: [], startMs: ms, endMs: ms, usage: ZERO };
      turns.push(turn);
    }
    return turn;
  };
  for (const e of events) {
    const ms = typeof e.ms === "number" ? e.ms : null;
    switch (e.event) {
      case "user":
        if (!e.hook) {
          turn = { rows: [], startMs: ms, endMs: ms, usage: ZERO };
          turns.push(turn);
        }
        current(ms).rows.push({ kind: "user", ms, text: e.text, context: e.context ?? [], hook: e.hook ?? null });
        break;
      case "assistant": {
        const t = current(ms);
        models.add(e.model);
        const step: Extract<TraceRow, { kind: "step" }> = {
          kind: "step", ms, startMs: prevMs, tookMs: ms !== null && prevMs !== null ? ms - prevMs : null, model: e.model, usage: e.usage,
          text: e.content.filter((b) => b.type === "text").map((b) => (b as { text: string }).text).join(""),
          thinking: e.content.filter((b) => b.type === "thinking").map((b) => (b as { text: string }).text).join(""),
          calls: [],
        };
        for (const b of e.content) {
          if (b.type !== "tool_use") continue;
          const c: TraceCall = { id: b.id, name: b.name, input: b.input, output: null, isError: false, startMs: null, tookMs: null, approval: null };
          calls.set(b.id, c);
          step.calls.push(c);
        }
        t.usage = plus(t.usage, e.usage);
        t.rows.push(step);
        lastStep = step;
        break;
      }
      case "tool_results": {
        const t = current(ms);
        t.usage = plus(t.usage, e.usage);
        e.results.forEach((r, i) => {
          if (r.type !== "tool_result") return;
          const c = calls.get(r.tool_use_id);
          if (!c) return;
          c.output = r.content;
          c.isError = !!r.is_error;
          c.tookMs = e.took_ms?.[i] ?? null;
        });
        if (lastStep) placeCalls(lastStep);
        break;
      }
      case "approval": {
        const c = calls.get(e.tool_use_id);
        if (c) c.approval = { allowed: e.allowed, by: e.by, ms };
        break;
      }
      case "compacted": {
        const t = current(ms);
        t.usage = plus(t.usage, e.usage);
        t.rows.push({ kind: "compacted", ms, startMs: prevMs, tookMs: ms !== null && prevMs !== null ? ms - prevMs : null, usage: e.usage, tokensBefore: e.tokens_before, summary: e.summary });
        break;
      }
      case "interrupted":
        current(ms).rows.push({ kind: "interrupted", ms, reason: e.reason });
        break;
    }
    if (ms !== null) {
      prevMs = ms;
      if (turn) (turn as TraceTurn).endMs = ms;
    }
  }
  const usage = turns.reduce((u, t) => plus(u, t.usage), ZERO);
  const steps = turns.reduce((n, t) => n + t.rows.filter((r) => r.kind === "step").length, 0);
  const timed = turns.filter((t) => t.startMs !== null && t.endMs !== null);
  return {
    turns, usage, steps, calls: calls.size, models: [...models],
    busyMs: timed.length ? timed.reduce((n, t) => n + (t.endMs! - t.startMs!), 0) : null,
  };
}

/** When each call of a step started: helpers (`subagent`) all at once when the step ended, the
 *  other calls one after another after them (the order the harness runs them in). */
function placeCalls(step: Extract<TraceRow, { kind: "step" }>) {
  if (step.ms === null) return;
  const helpers = step.calls.filter((c) => c.name === "subagent");
  let at = step.ms + Math.max(0, ...helpers.map((c) => c.tookMs ?? 0));
  for (const c of helpers) c.startMs = step.ms;
  for (const c of step.calls) {
    if (c.name === "subagent") continue;
    c.startMs = at;
    at += c.tookMs ?? 0;
  }
}
