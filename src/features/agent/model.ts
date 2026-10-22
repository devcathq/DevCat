// DevCat Agent chats without the window: how a session log and the live `agent-event`s become the
// items a chat draws, and how those items read (step rows, folded runs of steps). Shared by the
// desktop chat (./AgentChat.tsx) and the phone / browser version (mobile/src/screens/agentchat.tsx),
// so nothing here may import Tauri.
import { Boxes, Database, FilePen, FilePlus, FileText, FolderSearch, Globe, ListTodo, MessageCircleQuestion, Plug, ScrollText, Search, SquareTerminal, Wrench } from "lucide-react";
import type { AgentApprovalRequest, AgentBlock, AgentContext, AgentLiveEvent, AgentModel, AgentPolicy, AgentQuestion, AgentSessionEvent, AgentUsage } from "../../lib/types";

export type ToolStatus = "pending" | "approval" | "question" | "running" | "done" | "error" | "denied";

export type Item =
  | { kind: "user"; key: string; text: string; attachments: string[] }
  | { kind: "answer"; key: string; text: string; thinking: string; streaming: boolean; thinkingMs: number | null; thinkingStart: number | null }
  | { kind: "tool"; key: string; id: string; name: string; input: Record<string, unknown> | null; status: ToolStatus; output: string; request: AgentApprovalRequest | null; question?: AgentQuestion | null }
  | { kind: "end"; key: string; end: "answered" | "stepLimit" | "stopped" | "error"; error: string | null; elapsedMs: number | null; tokens: number }
  /** The conversation before it was summarized (automatically, or `/compact`). */
  | { kind: "compacted"; key: string; state: "running" | "done" | "failed"; summary: string; tokensBefore: number };

export type ToolItem = Extract<Item, { kind: "tool" }>;

/** What the live events change in a chat. */
export interface ChatView {
  title: string;
  busy: boolean;
  error: string | null;
  items: Item[];
  usage: AgentUsage;
  context: AgentContext | null;
  /** Input tokens of the last model call: how much of the context window the conversation fills. */
  contextTokens: number;
}

export const ZERO: AgentUsage = { input_tokens: 0, output_tokens: 0, cache_read_tokens: 0 };

export const POLICIES: { id: AgentPolicy; label: string; hint: string }[] = [
  { id: "ask", label: "Ask first", hint: "Ask before every file edit and command" },
  { id: "auto-edit", label: "Auto-edit", hint: "Edit files in the project without asking; ask before commands" },
  { id: "bypass", label: "Full access", hint: "Never ask: edits and commands run right away" },
];

export const SUGGESTIONS = ["Explain how this project is organized", "Find and fix failing tests", "Review my uncommitted changes for bugs"];

let seq = 0;
const key = () => `i${++seq}`;

/** Items for a session log (an opened or resumed chat). */
export function itemsFromLog(events: AgentSessionEvent[]): Item[] {
  const items: Item[] = [];
  const tools = new Map<string, ToolItem>();
  for (const e of events) {
    if (e.event === "user") items.push({ kind: "user", key: key(), ...splitAttachments(e.text) });
    else if (e.event === "assistant") addBlocks(items, tools, e.content);
    else if (e.event === "tool_results") {
      for (const r of e.results) if (r.type === "tool_result") finishTool(tools.get(r.tool_use_id), r.content, !!r.is_error);
    } else if (e.event === "approval" && !e.allowed) {
      const t = tools.get(e.tool_use_id);
      if (t) t.status = "denied";
    } else if (e.event === "compacted") {
      items.push({ kind: "compacted", key: key(), state: "done", summary: e.summary, tokensBefore: e.tokens_before });
    } else if (e.event === "interrupted") {
      items.push({ kind: "end", key: key(), end: e.reason.startsWith("Stopped by") ? "stopped" : "error", error: e.reason.startsWith("Stopped by") ? null : e.reason, elapsedMs: null, tokens: 0 });
    }
  }
  return items;
}

/** Context used by the last model call in a session log. */
export function lastContext(events: AgentSessionEvent[]): number {
  for (let i = events.length - 1; i >= 0; i--) {
    const e = events[i];
    if (e.event === "assistant") return e.usage.input_tokens + e.usage.output_tokens;
    if (e.event === "compacted") return Math.round(e.summary.length / 4);
  }
  return 0;
}

/** Context window of a model: what the provider's model list says, else a guess from its name.
 *  `exact` = known (listed, or a model in the table). */
export function contextWindow(model: string, listed: AgentModel[] = []): { tokens: number; exact: boolean } {
  const fromList = listed.find((x) => x.id === model)?.context;
  if (fromList) return { tokens: fromList, exact: true };
  const m = model.toLowerCase();
  const known: [RegExp, number][] = [
    [/claude.*\[1m\]|gpt-4\.1|gemini|glm-5\.[2-9]|glm-[6-9]|deepseek-v4|kimi-k3/, 1_048_576],
    [/gpt-5|o3|o4/, 400_000],
    [/kimi|moonshot/, 256_000],
    [/claude|glm-4\.[67]|glm-5/, 200_000],
    [/glm-4\.5|deepseek|gpt-4o|qwen3-coder/, 128_000],
    [/qwen3|llama|gemma|mistral/, 32_000],
  ];
  const hit = known.find(([re]) => re.test(m));
  return hit ? { tokens: hit[1], exact: true } : { tokens: 128_000, exact: false };
}

const ATTACHED = "\n\nAttached files:\n";

/** A logged user message carries its attachment list in the text: show them as chips again. */
function splitAttachments(text: string): { text: string; attachments: string[] } {
  const i = text.lastIndexOf(ATTACHED);
  if (i < 0) return { text, attachments: [] };
  const attachments = text.slice(i + ATTACHED.length).split("\n").map((l) => l.replace(/^- /, "").replace(/ \([^)]*\)$/, "").trim()).filter(Boolean);
  const rest = text.slice(0, i);
  return { text: rest === "See the attached files." ? "" : rest, attachments };
}

function addBlocks(items: Item[], tools: Map<string, ToolItem>, content: AgentBlock[]) {
  const text = content.filter((b) => b.type === "text").map((b) => (b as { text: string }).text).join("");
  const thinking = content.filter((b) => b.type === "thinking").map((b) => (b as { text: string }).text).join("");
  if (text.trim() || thinking.trim()) items.push({ kind: "answer", key: key(), text, thinking, streaming: false, thinkingMs: null, thinkingStart: null });
  for (const b of content) {
    if (b.type !== "tool_use") continue;
    const t: ToolItem = { kind: "tool", key: key(), id: b.id, name: b.name, input: b.input, status: "done", output: "", request: null };
    tools.set(b.id, t);
    items.push(t);
  }
}

function finishTool(t: ToolItem | undefined, content: string, isError: boolean) {
  if (!t) return;
  t.output = content;
  t.status = t.status === "denied" || content.startsWith("The user declined") ? "denied" : isError ? "error" : "done";
}

/** Apply one live event to a chat's items (a new array; items that change are replaced). */
export function apply(c: ChatView, e: AgentLiveEvent): Partial<ChatView> {
  const items = [...c.items];
  const last = items[items.length - 1];
  const now = Date.now();
  const toolIdx = (id: string) => items.findIndex((i) => i.kind === "tool" && i.id === id);
  const setTool = (id: string, f: (t: ToolItem) => Partial<ToolItem>) => {
    const i = toolIdx(id);
    if (i >= 0) items[i] = { ...(items[i] as ToolItem), ...f(items[i] as ToolItem) };
  };
  switch (e.type) {
    case "turnStart":
      items.push({ kind: "user", key: key(), text: e.text, attachments: e.attachments ?? [] });
      return { items, busy: true, error: null };
    case "context":
      return { context: e.context };
    case "question":
      if (toolIdx(e.question.toolUseId) < 0) items.push({ kind: "tool", key: key(), id: e.question.toolUseId, name: "ask_user", input: { question: e.question.question }, status: "question", output: "", request: null, question: e.question });
      else setTool(e.question.toolUseId, () => ({ status: "question", question: e.question }));
      return { items };
    case "thinking":
    case "text": {
      if (last?.kind === "answer" && last.streaming) {
        const a = { ...last };
        if (e.type === "thinking") a.thinking += e.text;
        else {
          if (a.thinkingStart && a.thinkingMs === null) a.thinkingMs = now - a.thinkingStart;
          a.text += e.text;
        }
        items[items.length - 1] = a;
      } else {
        const thinking = e.type === "thinking" ? e.text : "";
        items.push({ kind: "answer", key: key(), text: e.type === "text" ? e.text : "", thinking, streaming: true, thinkingMs: null, thinkingStart: thinking ? now : null });
      }
      return { items };
    }
    case "toolStart":
      if (last?.kind === "answer" && last.streaming && last.thinkingStart && last.thinkingMs === null) items[items.length - 1] = { ...last, thinkingMs: now - last.thinkingStart };
      if (toolIdx(e.id) < 0 && e.id) items.push({ kind: "tool", key: key(), id: e.id, name: e.name, input: null, status: "pending", output: "", request: null });
      return { items };
    case "step": {
      // The finished step: close the streamed answer, or add it when nothing was streamed.
      const streamed = items.findIndex((i) => i.kind === "answer" && i.streaming);
      if (streamed >= 0) {
        const a = items[streamed] as Extract<Item, { kind: "answer" }>;
        items[streamed] = { ...a, streaming: false, thinkingMs: a.thinkingMs ?? (a.thinkingStart ? now - a.thinkingStart : null) };
      } else {
        const tmp: Item[] = [];
        addBlocks(tmp, new Map(), e.content.filter((b) => b.type !== "tool_use"));
        items.push(...tmp);
      }
      for (const b of e.content) {
        if (b.type !== "tool_use") continue;
        if (toolIdx(b.id) >= 0) setTool(b.id, () => ({ input: b.input, name: b.name }));
        else items.push({ kind: "tool", key: key(), id: b.id, name: b.name, input: b.input, status: "pending", output: "", request: null });
      }
      const usage = { input_tokens: c.usage.input_tokens + e.usage.input_tokens, output_tokens: c.usage.output_tokens + e.usage.output_tokens, cache_read_tokens: c.usage.cache_read_tokens + e.usage.cache_read_tokens };
      return { items, usage, contextTokens: e.usage.input_tokens + e.usage.output_tokens || c.contextTokens };
    }
    case "approval":
      setTool(e.request.toolUseId, () => ({ status: "approval", request: e.request, input: e.request.input }));
      return { items };
    case "toolCall":
      setTool(e.id, (t) => ({ status: t.status === "question" ? "question" : "running", request: null, input: e.input }));
      return { items };
    case "compacting":
      items.push({ kind: "compacted", key: key(), state: "running", summary: "", tokensBefore: e.tokens_before });
      return { items };
    case "compacted": {
      const i = items.map((x) => x.kind === "compacted" && x.state === "running").lastIndexOf(true);
      const done = { kind: "compacted" as const, key: i >= 0 ? items[i].key : key(), state: "done" as const, summary: e.summary, tokensBefore: e.tokens_before };
      if (i >= 0) items[i] = done;
      else items.push(done);
      const usage = { input_tokens: c.usage.input_tokens + e.usage.input_tokens, output_tokens: c.usage.output_tokens + e.usage.output_tokens, cache_read_tokens: c.usage.cache_read_tokens + e.usage.cache_read_tokens };
      return { items, usage, contextTokens: Math.round(e.summary.length / 4) };
    }
    case "toolResult":
      setTool(e.id, (t) => ({
        output: e.content,
        request: null,
        question: null,
        status: t.status === "approval" || e.content.startsWith("The user declined") ? "denied" : e.is_error ? "error" : "done",
      }));
      return { items };
    case "turnEnd": {
      const closed = items.map((i): Item => (i.kind === "answer" && i.streaming ? { ...i, streaming: false }
        : i.kind === "tool" && (i.status === "pending" || i.status === "running" || i.status === "approval" || i.status === "question") ? { ...i, status: "error" as const, request: null, question: null }
        : i.kind === "compacted" && i.state === "running" ? { ...i, state: "failed" }
        : i));
      const tokens = e.usage.input_tokens + e.usage.output_tokens;
      closed.push({ kind: "end", key: key(), end: e.end, error: e.error, elapsedMs: e.elapsedMs, tokens });
      return { items: closed, busy: false, usage: e.usage, title: e.title };
    }
  }
}

/** Apply several events in order. */
export function applyAll<C extends ChatView>(c: C, events: AgentLiveEvent[]): C {
  return events.reduce<C>((acc, e) => ({ ...acc, ...apply(acc, e) }), c);
}

// ------------------------------------------------------------------ how items read

export const str = (v: unknown) => (typeof v === "string" ? v : "");
const lines = (s: string) => (s ? s.replace(/\n$/, "").split("\n").length : 0);
export function basename(p: string): string {
  const parts = p.replace(/[\\/]+$/, "").split(/[\\/]/);
  return parts[parts.length - 1] || p;
}

/** `path` relative to the project folder when it's inside it (the rows stay short). */
export function rel(path: string, cwd?: string) {
  if (!cwd || !path) return path;
  const base = cwd.replace(/[\\/]+$/, "");
  const fold = (s: string) => s.replace(/\\/g, "/").toLowerCase();
  if (fold(path) === fold(base)) return ".";
  return fold(path).startsWith(fold(base) + "/") ? path.slice(base.length + 1) : path;
}

/** The agent's checklist in a `todo_write` call. */
export type Todo = { content: string; status: "pending" | "in_progress" | "completed" };
export const todoList = (input: Record<string, unknown> | null): Todo[] => (Array.isArray(input?.todos) ? (input.todos as Todo[]).filter((t) => t && typeof t.content === "string") : []);

/** How a tool step reads: icon, verb, target and changed lines. */
export function describe(name: string, input: Record<string, unknown> | null, status: ToolStatus, cwd?: string) {
  const done = status === "done" || status === "error" || status === "denied";
  const i = input ?? {};
  switch (name) {
    case "read": return { icon: FileText, verb: done ? "Read" : "Reading", target: rel(str(i.path), cwd) };
    case "write": return { icon: FilePlus, verb: done ? "Wrote" : "Writing", target: rel(str(i.path), cwd), plus: lines(str(i.content)), minus: 0 };
    case "edit": return { icon: FilePen, verb: done ? "Edited" : "Editing", target: rel(str(i.path), cwd), plus: lines(str(i.new_string)), minus: lines(str(i.old_string)) };
    case "glob": return { icon: FolderSearch, verb: done ? "Found files" : "Finding files", target: str(i.pattern) };
    case "grep": return { icon: Search, verb: done ? "Searched" : "Searching", target: str(i.pattern) };
    case "shell": return { icon: SquareTerminal, verb: done ? "Ran" : "Running", target: str(i.command) };
    case "ask_user": return { icon: MessageCircleQuestion, verb: done ? "Asked" : "Asking", target: str(i.question) };
    case "web_fetch": return { icon: Globe, verb: done ? "Fetched" : "Fetching", target: str(i.url) };
    case "devcat_connections": return { icon: Database, verb: done ? "Listed connections" : "Listing connections", target: "" };
    case "db_schema": return { icon: Database, verb: done ? "Read the schema of" : "Reading the schema of", target: [str(i.connection), str(i.table)].filter(Boolean).join(" · ") };
    case "db_query": return { icon: Database, verb: done ? "Queried" : "Querying", target: `${str(i.connection)}: ${str(i.query).replace(/\s+/g, " ")}` };
    case "k8s_get": return { icon: Boxes, verb: done ? "Got" : "Getting", target: `${str(i.connection)}: ${[str(i.kind), str(i.name)].filter(Boolean).join(" ")}${i.namespace ? ` -n ${str(i.namespace)}` : ""}` };
    case "k8s_logs": return { icon: ScrollText, verb: done ? "Read logs of" : "Reading logs of", target: `${str(i.connection)}: ${str(i.namespace)}/${str(i.pod)}` };
    case "todo_write": {
      const todos = todoList(i);
      return { icon: ListTodo, verb: "Updated the plan", target: todos.length ? `${todos.filter((t) => t.status === "completed").length}/${todos.length} done` : "" };
    }
    default:
      if (name.startsWith("mcp__")) {
        const [, server, tool] = name.split("__");
        return { icon: Plug, verb: done ? "Used" : "Using", target: `${server} · ${tool}` };
      }
      return { icon: Wrench, verb: name, target: "" };
  }
}

export function fmtTokens(n: number) {
  return n >= 1_000_000 ? `${(n / 1_000_000).toFixed(1)}M` : n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n);
}

export function fmtDuration(ms: number) {
  const s = Math.max(1, Math.round(ms / 1000));
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${s % 60}s`;
}

/** A step the conversation folds away: a tool call that needs nothing from the user, or reasoning
 *  that came without an answer. */
function isStep(it: Item) {
  if (it.kind === "tool") return !(it.status === "approval" && it.request) && !(it.status === "question" && it.question);
  return it.kind === "answer" && !it.text.trim();
}

export type Block = { kind: "item"; item: Item; n: number } | { kind: "steps"; items: Item[]; last: number };

/** Runs of two or more steps become one foldable group; everything else shows as it is. */
export function toBlocks(items: Item[]): Block[] {
  const out: Block[] = [];
  let run: { items: Item[]; start: number } | null = null;
  const flush = () => {
    if (!run) return;
    if (run.items.length > 1) out.push({ kind: "steps", items: run.items, last: run.start + run.items.length - 1 });
    else out.push({ kind: "item", item: run.items[0], n: run.start });
    run = null;
  };
  items.forEach((it, n) => {
    if (isStep(it)) {
      if (run) run.items.push(it);
      else run = { items: [it], start: n };
    } else {
      flush();
      out.push({ kind: "item", item: it, n });
    }
  });
  flush();
  return out;
}

/** "read 6 files · 14 searches · ran 3 commands · 2 failed" for a group of steps. */
export function summarize(items: Item[]) {
  const reads = new Set<string>(), edits = new Set<string>();
  let searches = 0, commands = 0, tools = 0, fetches = 0, queries = 0, failed = 0, thinkingMs = 0;
  for (const it of items) {
    if (it.kind === "answer") thinkingMs += it.thinkingMs ?? 0;
    if (it.kind !== "tool") continue;
    if (it.status === "error") failed++;
    const target = str(it.input?.path) || str(it.input?.pattern);
    if (it.name === "read") reads.add(target);
    else if (it.name === "edit" || it.name === "write") edits.add(target);
    else if (it.name === "glob" || it.name === "grep") searches++;
    else if (it.name === "shell") commands++;
    else if (it.name === "web_fetch") fetches++;
    else if (it.name === "db_query" || it.name === "db_schema" || it.name === "k8s_get" || it.name === "k8s_logs") queries++;
    else if (it.name === "devcat_connections") continue;
    else if (it.name !== "todo_write") tools++;
  }
  const n = (k: number, one: string, many = `${one}s`) => `${k} ${k === 1 ? one : many}`;
  const parts = [
    reads.size && `read ${n(reads.size, "file")}`,
    edits.size && `edited ${n(edits.size, "file")}`,
    searches && n(searches, "search", "searches"),
    commands && `ran ${n(commands, "command")}`,
    fetches && `fetched ${n(fetches, "page")}`,
    queries && n(queries, "query", "queries"),
    tools && `used ${n(tools, "tool")}`,
  ].filter(Boolean) as string[];
  if (!parts.length && thinkingMs) parts.push(`thought for ${fmtDuration(thinkingMs)}`);
  return { text: parts.join(" · "), failed, explored: !edits.size && !commands && !tools && !queries };
}

/** Files a chat changed, with lines added / removed. */
export function editedFiles(items: Item[]) {
  const m = new Map<string, { plus: number; minus: number }>();
  for (const it of items) {
    if (it.kind !== "tool" || it.status !== "done" || (it.name !== "edit" && it.name !== "write")) continue;
    const d = describe(it.name, it.input, it.status);
    const f = m.get(d.target) ?? { plus: 0, minus: 0 };
    m.set(d.target, { plus: f.plus + (d.plus ?? 0), minus: f.minus + (d.minus ?? 0) });
  }
  return [...m.entries()];
}

/** Lines of a diff for an edit (`before` → `after`), removed first. */
export function diffRows(before: string, after: string) {
  const del = before ? before.replace(/\n$/, "").split("\n").map((l) => ["-", l] as const) : [];
  const add = after ? after.replace(/\n$/, "").split("\n").map((l) => ["+", l] as const) : [];
  return [...del, ...add];
}
