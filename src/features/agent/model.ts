// DevCat Agent chats without the window: how a session log and the live `agent-event`s become the
// items a chat draws, and how those items read (step rows, folded runs of steps). Shared by the
// desktop chat (./AgentChat.tsx) and the phone / browser version (mobile/src/screens/agentchat.tsx),
// so nothing here may import Tauri.
import { Bot, Boxes, Database, DatabaseBackup, DatabaseZap, FileDown, FilePen, FilePlus, FileText, FolderSearch, GitBranch, Globe, ListTodo, MessageCircleQuestion, NotebookPen, Plug, ScrollText, Search, SquareTerminal, StickyNote, Wrench } from "lucide-react";
import type { AgentApprovalRequest, AgentBlock, AgentContext, AgentLiveEvent, AgentModel, AgentPolicy, AgentQuestion, AgentSessionEvent, AgentUsage } from "../../lib/types";

export type ToolStatus = "pending" | "approval" | "question" | "running" | "done" | "error" | "denied";

export type Item =
  /** `queued`: sent while the agent worked, not read yet. */
  | { kind: "user"; key: string; text: string; attachments: string[]; queued?: boolean }
  | { kind: "answer"; key: string; text: string; thinking: string; streaming: boolean; thinkingMs: number | null; thinkingStart: number | null }
  /** `helper`: for a `subagent` call, the tool calls its helper made (live only; they're in the
   *  helper's own session). */
  | { kind: "tool"; key: string; id: string; name: string; input: Record<string, unknown> | null; status: ToolStatus; output: string; request: AgentApprovalRequest | null; question?: AgentQuestion | null; helper?: HelperStep[] }
  | { kind: "end"; key: string; end: "answered" | "stepLimit" | "stopped" | "error"; error: string | null; elapsedMs: number | null; tokens: number }
  /** The conversation before it was summarized (automatically, or `/compact`). */
  | { kind: "compacted"; key: string; state: "running" | "done" | "failed"; summary: string; tokensBefore: number }
  /** A hook's message: its `systemMessage`, a failure, or why a Stop hook sent the agent on. */
  | { kind: "hook"; key: string; event: string; text: string };

export type ToolItem = Extract<Item, { kind: "tool" }>;
export type HelperStep = { name: string; input: Record<string, unknown> | null };

/** The tool that hands a question to a helper (the harness runs it: crates/harness/src/agent.rs). */
export const SUBAGENT = "subagent";

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
  /** Messages it didn't read (it stopped), back for the chat box. */
  draft?: string | null;
}

export const ZERO: AgentUsage = { input_tokens: 0, output_tokens: 0, cache_read_tokens: 0 };

export const POLICIES: { id: AgentPolicy; label: string; hint: string }[] = [
  { id: "plan", label: "Plan", hint: "Look around and propose a plan; nothing is edited or run until you start it" },
  { id: "ask", label: "Ask first", hint: "Ask before every file edit and command" },
  { id: "auto-edit", label: "Auto-edit", hint: "Edit files in the project without asking; ask before commands" },
  { id: "bypass", label: "Full access", hint: "Never ask: edits and commands run right away" },
];

export const SUGGESTIONS = ["Explain how this project is organized", "Find and fix failing tests", "Review my uncommitted changes for bugs"];

/** What to ask a ghost chat (no project: a fresh conversation). */
export const GHOST_SUGGESTIONS = ["Explain a concept to me", "Help me draft a message", "Think through a decision with me"];

/** What to ask a chat opened from a connection, by the connection's kind (`AgentContext.focusKind`). */
export function focusSuggestions(kind: string | null | undefined): string[] {
  if (kind === "kubernetes") return ["What's unhealthy in this cluster?", "Which pods restarted recently, and why?", "Summarize the deployments in each namespace"];
  if (kind === "redis") return ["What kinds of keys are in here?", "Which keys take the most memory?", "Find keys without an expiry"];
  if (kind === "mongodb") return ["Explain how these collections fit together", "Dump this database to a file", "Export a collection to JSON"];
  return ["Explain how these tables fit together", "Dump this database to a file", "Export a table to CSV"];
}

let seq = 0;
const key = () => `i${++seq}`;

/** Items for a session log (an opened or resumed chat). */
export function itemsFromLog(events: AgentSessionEvent[]): Item[] {
  const items: Item[] = [];
  const tools = new Map<string, ToolItem>();
  for (const e of events) {
    if (e.event === "user" && e.hook) items.push({ kind: "hook", key: key(), event: e.hook, text: e.text });
    else if (e.event === "user") items.push({ kind: "user", key: key(), ...splitAttachments(e.text) });
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

/** A result saying the call was declined: by the user, or by a PermissionRequest hook in their place. */
const declined = (content: string) => content.startsWith("The user declined") || content.startsWith("Not run: a PermissionRequest hook declined");

function finishTool(t: ToolItem | undefined, content: string, isError: boolean) {
  if (!t) return;
  t.output = content;
  t.status = t.status === "denied" || declined(content) ? "denied" : isError ? "error" : "done";
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
    case "queued":
      items.push({ kind: "user", key: key(), text: e.text, attachments: e.attachments ?? [], queued: true });
      return { items };
    case "userMessage": {
      // the oldest one waiting is the one it read (or a hook refused: it goes, the hook's note says why)
      const i = items.findIndex((x) => x.kind === "user" && x.queued);
      if (i >= 0 && e.blocked) items.splice(i, 1);
      else if (i >= 0) items[i] = { ...(items[i] as Extract<Item, { kind: "user" }>), queued: false };
      return { items };
    }
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
      return { items, usage: plus(c.usage, e.usage), contextTokens: e.usage.input_tokens + e.usage.output_tokens || c.contextTokens };
    }
    case "helperTool":
      setTool(e.id, (t) => ({ helper: [...(t.helper ?? []), { name: e.name, input: e.input }] }));
      return { items };
    case "helperStep":
      // a helper's tokens count for the chat, not for its context
      return { usage: plus(c.usage, e.usage) };
    case "hook":
      items.push({ kind: "hook", key: key(), event: e.event, text: e.message });
      return { items };
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
      return { items, usage: plus(c.usage, e.usage), contextTokens: Math.round(e.summary.length / 4) };
    }
    case "toolResult":
      setTool(e.id, (t) => ({
        output: e.content,
        request: null,
        question: null,
        status: t.status === "approval" || declined(e.content) ? "denied" : e.is_error ? "error" : "done",
      }));
      return { items };
    case "turnEnd": {
      // what it didn't read goes back to the chat box
      const draft = e.unsent?.length ? e.unsent.map((t) => splitAttachments(t).text).join("\n\n") : null;
      const closed = items.filter((i) => !(i.kind === "user" && i.queued)).map((i): Item => (i.kind === "answer" && i.streaming ? { ...i, streaming: false }
        : i.kind === "tool" && (i.status === "pending" || i.status === "running" || i.status === "approval" || i.status === "question") ? { ...i, status: "error" as const, request: null, question: null }
        : i.kind === "compacted" && i.state === "running" ? { ...i, state: "failed" }
        : i));
      const tokens = e.usage.input_tokens + e.usage.output_tokens;
      closed.push({ kind: "end", key: key(), end: e.end, error: e.error, elapsedMs: e.elapsedMs, tokens });
      return { items: closed, busy: false, usage: e.usage, title: e.title, ...(draft ? { draft } : {}) };
    }
  }
}

const plus = (a: AgentUsage, b: AgentUsage): AgentUsage => ({ input_tokens: a.input_tokens + b.input_tokens, output_tokens: a.output_tokens + b.output_tokens, cache_read_tokens: a.cache_read_tokens + b.cache_read_tokens });

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

/** The `git` tool's arguments. */
const gitArgs = (input: Record<string, unknown> | null) => (Array.isArray(input?.args) ? input.args.map(String) : []);

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
    case "git": return { icon: GitBranch, verb: done ? "Ran" : "Running", target: `git ${gitArgs(i).map((a) => (/^$|\s/.test(a) ? `"${a}"` : a)).join(" ")}` };
    case "ask_user": return { icon: MessageCircleQuestion, verb: done ? "Asked" : "Asking", target: str(i.question) };
    case SUBAGENT: return { icon: Bot, verb: done ? "Asked a helper" : "Asking a helper", target: str(i.description) || str(i.prompt).split("\n")[0] };
    case "web_fetch": return { icon: Globe, verb: done ? "Fetched" : "Fetching", target: str(i.url) };
    case "devcat_connections": return { icon: Database, verb: done ? "Listed connections" : "Listing connections", target: "" };
    case "db_schema": return { icon: Database, verb: done ? "Read the schema of" : "Reading the schema of", target: [str(i.connection), str(i.table)].filter(Boolean).join(" · ") };
    case "db_query": return { icon: Database, verb: done ? "Queried" : "Querying", target: `${str(i.connection)}: ${str(i.query).replace(/\s+/g, " ")}` };
    case "db_dump": return { icon: DatabaseBackup, verb: done ? "Dumped" : "Dumping", target: [str(i.connection), str(i.database)].filter(Boolean).join(" · ") + (i.path ? ` → ${rel(str(i.path), cwd)}` : "") };
    case "db_export": return { icon: FileDown, verb: done ? "Exported" : "Exporting", target: `${str(i.connection)}: ${str(i.table) || str(i.query).replace(/\s+/g, " ")}` };
    case "db_drop_database": return { icon: DatabaseZap, verb: done ? "Dropped database" : "Dropping database", target: `${str(i.database)} on ${str(i.connection)}` };
    case "k8s_get": return { icon: Boxes, verb: done ? "Got" : "Getting", target: `${str(i.connection)}: ${[str(i.kind), str(i.name)].filter(Boolean).join(" ")}${i.namespace ? ` -n ${str(i.namespace)}` : ""}` };
    case "k8s_logs": return { icon: ScrollText, verb: done ? "Read logs of" : "Reading logs of", target: `${str(i.connection)}: ${str(i.namespace)}/${str(i.pod)}` };
    case "devcat_notes": return i.id
      ? { icon: StickyNote, verb: done ? "Read a note" : "Reading a note", target: str(i.id) }
      : { icon: StickyNote, verb: done ? "Searched notes" : "Searching notes", target: [str(i.query), str(i.project)].filter(Boolean).join(" · ") };
    case "devcat_note_write": return { icon: NotebookPen, verb: done ? (i.id ? "Updated a note" : "Saved a note") : "Saving a note", target: noteTitle(str(i.body)) };
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

/** What a helper is doing, under its card: "4 steps · Reading src/lib/api.ts". */
export function helperProgress(steps: HelperStep[], cwd?: string) {
  const last = steps[steps.length - 1];
  if (!last) return "";
  const d = describe(last.name, last.input, "running", cwd);
  return `${steps.length} ${steps.length === 1 ? "step" : "steps"} · ${d.verb} ${d.target}`.trim();
}

/** What a chat's tokens cost at a model's list price (US dollars); null without a price. Cached
 *  input is part of `input_tokens`, at the cache price when there is one. */
export function chatCost(u: AgentUsage, price: AgentModel["price"]): number | null {
  if (!price) return null;
  const cached = Math.min(u.cache_read_tokens, u.input_tokens);
  return ((u.input_tokens - cached) * price.input + cached * (price.cacheRead ?? price.input) + u.output_tokens * price.output) / 1_000_000;
}

export function fmtTokens(n: number) {
  return n >= 1_000_000 ? `${(n / 1_000_000).toFixed(1)}M` : n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n);
}

export function fmtDuration(ms: number) {
  const s = Math.max(1, Math.round(ms / 1000));
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${s % 60}s`;
}

/** A step the conversation folds away: a tool call that needs nothing from the user (helpers stay
 *  in view: they run side by side, and their answers matter), or reasoning that came without an
 *  answer. */
function isStep(it: Item) {
  if (it.kind === "tool") return it.name !== SUBAGENT && !(it.status === "approval" && it.request) && !(it.status === "question" && it.question);
  // a hook's note folds in with the steps around it, except a Stop hook sending the agent on
  if (it.kind === "hook") return !isStopHook(it.event);
  return it.kind === "answer" && !it.text.trim();
}

export const isStopHook = (event: string) => event === "Stop" || event === "SubagentStop";

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

/** `git` subcommands that only read the repository (the harness decides what asks: tools/git.rs;
 *  this only sorts them for the summary line). */
const GIT_READS = new Set(["status", "diff", "log", "show", "blame", "grep", "ls-files", "rev-parse", "describe", "shortlog", "merge-base"]);

/** What "Always allow … in this chat" covers for a tool: the whole tool, for the rest of the chat. */
export function allowAllLabel(name: string) {
  if (name === "shell") return "commands";
  if (name === "git") return "git commands";
  if (name === "devcat_notes") return "reading notes";
  if (name === "devcat_note_write") return "saving notes";
  if (name === "web_fetch") return "fetching pages";
  if (name === "db_query") return "queries";
  if (name === "db_dump") return "dumps";
  if (name === "db_export") return "exports";
  if (name.startsWith("k8s_")) return "cluster reads";
  if (name.startsWith("mcp__")) return "this tool";
  return "edits";
}

/** The question on an approval card: what the call is about to do. */
export function approvalTitle(name: string, kind: AgentApprovalRequest["kind"], input: Record<string, unknown> | null) {
  const i = input ?? {};
  if (name === "db_query") return kind === "query" ? `Read from ${str(i.connection)}?` : `Run this on ${str(i.connection)}? It may change data`;
  if (name === "db_dump") return `Dump ${str(i.database) || str(i.connection)} to a file?`;
  if (name === "db_export") return `Export from ${str(i.connection)} to a file?`;
  if (name === "db_drop_database") return `Drop the database ${str(i.database)} on ${str(i.connection)}? This can't be undone`;
  if (name === "devcat_notes") return "Read your DevCat notes?";
  if (name === "devcat_note_write") return i.id ? "Change a DevCat note?" : "Save a new DevCat note?";
  if (kind === "query") return `Read from ${str(i.connection)}?`;
  if (kind === "exec") return "Run this command?";
  if (kind === "fetch") return "Fetch this page?";
  if (kind === "edit-outside") return "Edit a file outside the project?";
  return `${name === "write" ? "Write" : "Edit"} ${basename(str(i.path))}?`;
}

/** A note's title: its first non-empty line without Markdown markers. The same as `noteTitle` in
 *  features/notes/store.ts, which this file can't import (it uses Tauri; this one is shared with the phone). */
function noteTitle(body: string) {
  const line = body.split("\n").map((l) => l.replace(/^\s*(#{1,6}\s+|[-*+]\s+(\[[ xX]\]\s+)?|>\s*)/, "").trim()).find(Boolean);
  return line ? line.slice(0, 120) : "Untitled note";
}

/** "read 6 files · 14 searches · ran 3 commands · 2 failed" for a group of steps. */
export function summarize(items: Item[]) {
  const reads = new Set<string>(), edits = new Set<string>();
  let searches = 0, commands = 0, gitReads = 0, noteReads = 0, noteWrites = 0, tools = 0, fetches = 0, queries = 0, failed = 0, thinkingMs = 0, hookNotes = 0;
  for (const it of items) {
    if (it.kind === "answer") thinkingMs += it.thinkingMs ?? 0;
    if (it.kind === "hook") hookNotes++;
    if (it.kind !== "tool") continue;
    if (it.status === "error") failed++;
    const target = str(it.input?.path) || str(it.input?.pattern);
    if (it.name === "read") reads.add(target);
    else if (it.name === "edit" || it.name === "write") edits.add(target);
    else if (it.name === "glob" || it.name === "grep") searches++;
    else if (it.name === "shell") commands++;
    else if (it.name === "git") {
      if (GIT_READS.has(gitArgs(it.input)[0])) gitReads++;
      else commands++;
    }
    else if (it.name === "web_fetch") fetches++;
    else if (it.name === "db_query" || it.name === "db_schema" || it.name === "k8s_get" || it.name === "k8s_logs") queries++;
    else if (it.name === "devcat_notes") noteReads++;
    else if (it.name === "devcat_note_write") noteWrites++;
    else if (it.name === "devcat_connections") continue;
    else if (it.name !== "todo_write") tools++;
  }
  const n = (k: number, one: string, many = `${one}s`) => `${k} ${k === 1 ? one : many}`;
  const parts = [
    reads.size && `read ${n(reads.size, "file")}`,
    edits.size && `edited ${n(edits.size, "file")}`,
    searches && n(searches, "search", "searches"),
    gitReads && n(gitReads, "git lookup"),
    noteReads && n(noteReads, "note lookup"),
    noteWrites && `saved ${n(noteWrites, "note")}`,
    commands && `ran ${n(commands, "command")}`,
    fetches && `fetched ${n(fetches, "page")}`,
    queries && n(queries, "query", "queries"),
    tools && `used ${n(tools, "tool")}`,
    hookNotes && n(hookNotes, "hook message"),
  ].filter(Boolean) as string[];
  if (!parts.length && thinkingMs) parts.push(`thought for ${fmtDuration(thinkingMs)}`);
  return { text: parts.join(" · "), failed, explored: !edits.size && !commands && !tools && !queries && !noteWrites };
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

/** The edited files git still sees changed (what the Diff view shows): files outside the
 *  repository, or committed or reverted since, drop out. `git` is the folder's status. */
export function stillChanged(files: [string, { plus: number; minus: number }][], git: { isRepo: boolean; root?: string | null; files: { path: string }[] } | null, cwd: string) {
  if (!git?.isRepo || !git.root) return [];
  const norm = (p: string) => {
    const s = p.replace(/\\/g, "/").replace(/\/+$/, "");
    return /^[a-z]:\//i.test(s) ? s.toLowerCase() : s;
  };
  const root = norm(git.root);
  const changed = new Set(git.files.map((f) => norm(`${root}/${f.path}`)));
  const base = norm(cwd);
  return files.filter(([p]) => {
    const abs = /^([a-z]:)?[\\/]/i.test(p) ? norm(p) : norm(`${base}/${p}`);
    return changed.has(abs);
  });
}

/** Lines of a diff for an edit (`before` → `after`), removed first. */
export function diffRows(before: string, after: string) {
  const del = before ? before.replace(/\n$/, "").split("\n").map((l) => ["-", l] as const) : [];
  const add = after ? after.replace(/\n$/, "").split("\n").map((l) => ["+", l] as const) : [];
  return [...del, ...add];
}
