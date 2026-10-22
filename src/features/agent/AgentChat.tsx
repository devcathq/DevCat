// DevCat Agent chat view (AI workspace tabs whose AI connection is a DevCat Agent). Laid out like
// DeepSeek Harness / Zed's agent panel: a centered conversation; each tool call is one compact step
// row (click for details); approvals are inline cards; reasoning is folded away; the composer
// carries the permission mode, model and token use.
import clsx from "clsx";
import {
  ArrowUp, Ban, BookOpen, Layers, Globe, ListTodo, Circle, CircleCheck, CircleDot, FoldVertical, Bot, Brain, Check, ChevronDown, ChevronRight, Cpu, FilePen, File as FileIcon, MessageCircleQuestion, Paperclip, Plug, TriangleAlert, Upload, FilePlus, FileText, FolderSearch, GitCompare, Search, Shield, ShieldAlert, ShieldCheck, ShieldOff, Square,
  SquareTerminal, Wrench, X,
} from "lucide-react";
import { memo, useDeferredValue, useEffect, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { Button, Empty, Spinner } from "../../components/ui";
import type { AgentModel, AgentPolicy, AiProvider } from "../../lib/types";
import { useTabs } from "../../store/tabs";
import { Markdown } from "../claude/Markdown";
import { openManageProviders } from "../claude/providers";
import { basename } from "../claude/util";
import { DRAG_PATHS } from "../files/format";
import { open as pickFiles } from "@tauri-apps/plugin-dialog";
import { answer, answerQuestion, attach, closeChat, connectionModels, contextWindow, dismissError, openChat, send, setModel, setPolicy, stop, unattach, useAgent, type Item, type ToolStatus } from "./store";

type ToolItem = Extract<Item, { kind: "tool" }>;

const POLICIES: { id: AgentPolicy; label: string; hint: string }[] = [
  { id: "ask", label: "Ask first", hint: "Ask before every file edit and command" },
  { id: "auto-edit", label: "Auto-edit", hint: "Edit files in the project without asking; ask before commands" },
  { id: "bypass", label: "Full access", hint: "Never ask: edits and commands run right away" },
];

const SUGGESTIONS = ["Explain how this project is organized", "Find and fix failing tests", "Review my uncommitted changes for bugs"];

const str = (v: unknown) => (typeof v === "string" ? v : "");
const lines = (s: string) => (s ? s.replace(/\n$/, "").split("\n").length : 0);

type Todo = { content: string; status: "pending" | "in_progress" | "completed" };
const todoList = (input: Record<string, unknown> | null): Todo[] => (Array.isArray(input?.todos) ? (input.todos as Todo[]).filter((t) => t && typeof t.content === "string") : []);

/** `path` relative to the project folder when it's inside it (the rows stay short). */
function rel(path: string, cwd?: string) {
  if (!cwd || !path) return path;
  const base = cwd.replace(/[\\/]+$/, "");
  const fold = (s: string) => s.replace(/\\/g, "/").toLowerCase();
  if (fold(path) === fold(base)) return ".";
  return fold(path).startsWith(fold(base) + "/") ? path.slice(base.length + 1) : path;
}

/** How a tool step reads: icon, verb, target and changed lines. */
function describe(name: string, input: Record<string, unknown> | null, status: ToolStatus, cwd?: string) {
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

function fmtTokens(n: number) {
  return n >= 1_000_000 ? `${(n / 1_000_000).toFixed(1)}M` : n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n);
}

function fmtDuration(ms: number) {
  const s = Math.max(1, Math.round(ms / 1000));
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${s % 60}s`;
}

export default function AgentChat({ tabId, cwd, provider, visible, onReview }: { tabId: string; cwd: string; provider: AiProvider; visible: boolean; onReview: () => void }) {
  const chat = useAgent((s) => s.chats[tabId]);
  const sessionId = useTabs((s) => {
    const t = s.tabs.find((x) => x.id === tabId);
    return t && t.type === "claude" ? t.sessionId ?? null : null;
  });

  // Open (or resume) the session; remember it on the tab so it comes back after a restart.
  useEffect(() => {
    if (useAgent.getState().chats[tabId]?.sessionId === sessionId && sessionId) return;
    void openChat(tabId, cwd, provider.id, sessionId).then((id) => {
      if (id && id !== sessionId) useTabs.getState().update(tabId, { sessionId: id } as never);
    });
  }, [tabId, cwd, provider.id, sessionId]);
  useEffect(() => () => void closeChat(tabId), [tabId]);

  if (!chat || (chat.opening && !chat.items.length)) {
    return <div className="flex h-full items-center justify-center"><Spinner className="size-5" /></div>;
  }
  if (!chat.sessionId && chat.error) {
    return (
      <Empty icon={<Bot className="size-8" />} title="DevCat Agent couldn't start">
        <div className="flex flex-col items-center gap-2">
          <p className="max-w-md text-danger">{chat.error}</p>
          <Button onClick={() => openManageProviders(provider.id)}>Edit {provider.name}</Button>
        </div>
      </Empty>
    );
  }
  return <ChatBody tabId={tabId} cwd={cwd} visible={visible} onReview={onReview} provider={provider} />;
}

function ChatBody({ tabId, cwd, visible, onReview, provider }: { tabId: string; cwd: string; visible: boolean; onReview: () => void; provider: AiProvider }) {
  const chat = useAgent((s) => s.chats[tabId])!;
  const scroller = useRef<HTMLDivElement>(null);
  const stick = useRef(true);

  // Follow new content while the view is at the bottom.
  useLayoutEffect(() => {
    const el = scroller.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [chat.items]);
  const onScroll = () => {
    const el = scroller.current;
    if (el) stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 120;
  };

  const lastUser = chat.items.map((i) => i.kind).lastIndexOf("user");
  const blocks = useMemo(() => toBlocks(chat.items), [chat.items]);
  const [dragging, setDragging] = useState(false);
  // Files from the system (their bytes) or from DevCat's Files tree (their paths).
  const hasFiles = (e: React.DragEvent) => [...e.dataTransfer.types].some((t) => t === "Files" || t === DRAG_PATHS);
  return (
    <div className="relative flex h-full flex-col"
      onDragOver={(e) => { if (hasFiles(e)) { e.preventDefault(); setDragging(true); } }}
      onDragLeave={(e) => { if (e.currentTarget === e.target || !e.currentTarget.contains(e.relatedTarget as Node)) setDragging(false); }}
      onDrop={(e) => {
        if (!hasFiles(e)) return;
        e.preventDefault();
        setDragging(false);
        const fromTree = e.dataTransfer.getData(DRAG_PATHS);
        if (fromTree) {
          try { void attach(tabId, JSON.parse(fromTree) as string[]); } catch { /* not ours */ }
        } else {
          void attach(tabId, [...e.dataTransfer.files]);
        }
      }}>
      {dragging && (
        <div className="pointer-events-none absolute inset-3 z-10 flex flex-col items-center justify-center gap-2 rounded-xl border-2 border-dashed border-accent/60 bg-app/80 text-[13px] text-fg">
          <Upload className="size-6 text-accent" />Drop files to attach them<span className="text-[11px] text-muted">Images are shown to the model; PDFs and other files it reads with its tools</span>
        </div>
      )}
      <div ref={scroller} onScroll={onScroll} className="min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto flex max-w-[780px] flex-col gap-3 px-5 pt-6 pb-4">
          {chat.items.length === 0 ? (
            <Welcome tabId={tabId} cwd={cwd} />
          ) : (
            blocks.map((b) => b.kind === "steps"
              ? <Steps key={b.items[0].key} items={b.items} tabId={tabId} cwd={cwd} live={chat.busy && b.last === chat.items.length - 1} working={chat.busy && b.last > lastUser} />
              : <ItemView key={b.item.key} item={b.item} tabId={tabId} cwd={cwd} working={chat.busy && b.n > lastUser} />)
          )}
          {chat.busy && blocks[blocks.length - 1]?.kind !== "steps" && <Working items={chat.items} />}
        </div>
      </div>
      {chat.error && (
        <div className="mx-auto flex w-full max-w-[780px] items-start gap-2 px-5">
          <div className="flex flex-1 items-start gap-2 rounded-md border border-danger/30 bg-danger/10 px-3 py-2 text-[12px] text-danger">
            <span className="flex-1">{chat.error}</span>
            <button className="text-danger/70 hover:text-danger" onClick={() => dismissError(tabId)} aria-label="Dismiss"><X className="size-3.5" /></button>
          </div>
        </div>
      )}
      <TodoPanel items={chat.items} busy={chat.busy} />
      <EditedFiles items={chat.items} onReview={onReview} />
      <Composer tabId={tabId} visible={visible} provider={provider} />
    </div>
  );
}

function Welcome({ tabId, cwd }: { tabId: string; cwd: string }) {
  const chat = useAgent((s) => s.chats[tabId])!;
  const policy = POLICIES.find((p) => p.id === chat.policy)!;
  return (
    <div className="flex flex-col items-center gap-4 pt-[12vh] text-center">
      <div className="flex size-11 items-center justify-center rounded-xl bg-accent/15 text-accent"><Bot className="size-6" /></div>
      <div>
        <h2 className="text-lg font-semibold text-fg">What should we do in {basename(cwd)}?</h2>
        <p className="mt-1 text-[12px] text-muted">{chat.providerName} · {chat.model} · {policy.hint.toLowerCase()}.</p>
      </div>
      <ContextLine tabId={tabId} />
      <div className="flex flex-wrap justify-center gap-2">
        {SUGGESTIONS.map((s) => (
          <button key={s} onClick={() => void send(tabId, s)} className="rounded-full border border-line px-3 py-1.5 text-[12px] text-muted hover:border-accent/50 hover:text-fg">{s}</button>
        ))}
      </div>
    </div>
  );
}

/** A step the conversation folds away: a tool call that needs nothing from the user, or reasoning
 *  that came without an answer. */
function isStep(it: Item) {
  if (it.kind === "tool") return !(it.status === "approval" && it.request) && !(it.status === "question" && it.question);
  return it.kind === "answer" && !it.text.trim();
}

type Block = { kind: "item"; item: Item; n: number } | { kind: "steps"; items: Item[]; last: number };

/** Runs of two or more steps become one foldable group; everything else shows as it is. */
function toBlocks(items: Item[]): Block[] {
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
function summarize(items: Item[]) {
  const reads = new Set<string>(), edits = new Set<string>();
  let searches = 0, commands = 0, tools = 0, fetches = 0, failed = 0, thinkingMs = 0;
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
    else if (it.name !== "todo_write") tools++;
  }
  const n = (k: number, one: string, many = `${one}s`) => `${k} ${k === 1 ? one : many}`;
  const parts = [
    reads.size && `read ${n(reads.size, "file")}`,
    edits.size && `edited ${n(edits.size, "file")}`,
    searches && n(searches, "search", "searches"),
    commands && `ran ${n(commands, "command")}`,
    fetches && `fetched ${n(fetches, "page")}`,
    tools && `used ${n(tools, "tool")}`,
  ].filter(Boolean) as string[];
  if (!parts.length && thinkingMs) parts.push(`thought for ${fmtDuration(thinkingMs)}`);
  return { text: parts.join(" · "), failed, explored: !edits.size && !commands && !tools };
}

/** A run of steps folded into one line; the step in progress shows under it while it runs. */
function Steps({ items, tabId, cwd, live, working }: { items: Item[]; tabId: string; cwd: string; live: boolean; working: boolean }) {
  const [open, setOpen] = useState(false);
  const s = summarize(items);
  const current = live ? items[items.length - 1] : null;
  return (
    <div className="flex flex-col gap-1">
      <button onClick={() => setOpen(!open)} aria-expanded={open}
        className="group flex w-full min-w-0 items-center gap-2 rounded-md px-1 py-0.5 text-left text-[12.5px] hover:bg-hover/60">
        {live ? <Spinner className="size-3.5 shrink-0" /> : <Layers className="size-3.5 shrink-0 text-muted" />}
        <span className="shrink-0 text-fg">{live ? "Working" : s.explored ? "Explored" : "Worked"}</span>
        <span className="min-w-0 truncate text-muted">{s.text}</span>
        {s.failed > 0 && <span className="shrink-0 text-[11.5px] text-danger">· {s.failed} failed</span>}
        <ChevronRight className={clsx("ml-auto size-3 shrink-0 text-faint transition", open && "rotate-90")} />
      </button>
      {open ? (
        <div className="ml-[7px] flex flex-col gap-1 border-l border-line pl-3">
          {items.map((it) => <ItemView key={it.key} item={it} tabId={tabId} cwd={cwd} working={working} />)}
        </div>
      ) : current && (
        <div className="ml-[7px] border-l border-line pl-3"><ItemView key={current.key} item={current} tabId={tabId} cwd={cwd} working={working} /></div>
      )}
    </div>
  );
}

/** "Thinking…" while the model hasn't started its answer or a tool yet. */
function Working({ items }: { items: Item[] }) {
  const last = items[items.length - 1];
  const quiet = !last || last.kind === "user" || (last.kind === "tool" && (last.status === "done" || last.status === "error" || last.status === "denied"));
  if (!quiet) return null;
  return <div className="flex items-center gap-2 text-[12px] text-muted"><Spinner className="size-3.5" />Working…</div>;
}

const ItemView = memo(function ItemView({ item, tabId, cwd, working }: { item: Item; tabId: string; cwd: string; working: boolean }) {
  switch (item.kind) {
    case "user":
      return (
        <div className="flex flex-col items-end gap-1.5">
          {item.attachments.length > 0 && <div className="flex max-w-[85%] flex-wrap justify-end gap-1.5">{item.attachments.map((p) => <AttachmentChip key={p} path={p} />)}</div>}
          {item.text && <div className="max-w-[85%] rounded-2xl rounded-br-md bg-raised px-3.5 py-2 text-[13px] whitespace-pre-wrap text-fg">{item.text}</div>}
        </div>
      );
    case "answer":
      return <Answer item={item} cwd={cwd} />;
    case "tool":
      return <ToolRow item={item} tabId={tabId} cwd={cwd} working={working} />;
    case "end":
      return <TurnEnd item={item} />;
    case "compacted":
      return <CompactedRow item={item} cwd={cwd} />;
  }
});

function AttachmentChip({ path, onRemove }: { path: string; onRemove?: () => void }) {
  return (
    <span className="flex h-7 max-w-[240px] items-center gap-1.5 rounded-md border border-line bg-panel px-2 text-[12px] text-muted" title={path}>
      <FileIcon className="size-3.5 shrink-0" /><span className="truncate">{basename(path)}</span>
      {onRemove && <button onClick={onRemove} className="text-faint hover:text-fg" aria-label={`Remove ${basename(path)}`}><X className="size-3" /></button>}
    </span>
  );
}

/** What the agent loaded for this folder: rules, skills, MCP servers (and problems). */
function ContextLine({ tabId }: { tabId: string }) {
  const ctx = useAgent((s) => s.chats[tabId]?.context);
  if (!ctx) return null;
  const parts: ReactNode[] = [];
  if (ctx.rules.length) parts.push(<span key="r" title={ctx.rules.join("\n")} className="flex items-center gap-1"><BookOpen className="size-3" />{ctx.rules.length === 1 ? ctx.rules[0] : `${ctx.rules.length} rule files`}</span>);
  if (ctx.skills.length) parts.push(<span key="s" title={ctx.skills.join("\n")}>{ctx.skills.length} skill{ctx.skills.length === 1 ? "" : "s"}</span>);
  for (const m of ctx.mcp) parts.push(<span key={`m${m.name}`} className="flex items-center gap-1"><Plug className="size-3" />{m.name} ({m.tools} tool{m.tools === 1 ? "" : "s"})</span>);
  if (ctx.mcpLoading) parts.push(<span key="ml" className="flex items-center gap-1"><Spinner className="size-3" />connecting MCP servers…</span>);
  return (
    <div className="flex flex-col items-center gap-1 text-[11.5px] text-faint">
      {parts.length > 0 && <div className="flex flex-wrap items-center justify-center gap-x-3 gap-y-1"><span>Using</span>{parts}</div>}
      {ctx.notices.map((n) => <div key={n} className="flex items-start gap-1.5 text-warn"><TriangleAlert className="mt-0.5 size-3 shrink-0" />{n}</div>)}
    </div>
  );
}

function Answer({ item, cwd }: { item: Extract<Item, { kind: "answer" }>; cwd: string }) {
  // Shown while the model thinks, folded once it answers; a click always wins.
  const [choice, setChoice] = useState<boolean | null>(null);
  const thinkingLive = item.streaming && !item.text;
  const open = choice ?? thinkingLive;
  // While streaming, the Markdown re-render runs at low priority (typing and scrolling stay smooth).
  const text = useDeferredValue(item.text);
  const thinking = useDeferredValue(item.thinking);
  return (
    <div className="flex flex-col gap-1.5">
      {item.thinking.trim() && (
        <div>
          <button className="flex items-center gap-1.5 text-[12px] text-muted hover:text-fg" onClick={() => setChoice(!open)} aria-expanded={open}>
            <Brain className="size-3.5" />
            {thinkingLive ? <span className="animate-pulse">Thinking…</span> : item.thinkingMs ? `Thought for ${fmtDuration(item.thinkingMs)}` : "Thought"}
            <ChevronRight className={clsx("size-3 transition-transform", open && "rotate-90")} />
          </button>
          {open && (
            <div className="mt-1.5 max-h-64 overflow-y-auto border-l-2 border-line pl-3 text-[12px] leading-relaxed whitespace-pre-wrap text-muted">{thinking.trim()}</div>
          )}
        </div>
      )}
      {text.trim() && <Markdown text={text} cwd={cwd} className="text-[13px]" />}
    </div>
  );
}

function StatusIcon({ status }: { status: ToolStatus }) {
  switch (status) {
    case "pending":
    case "running": return <Spinner className="size-3.5" />;
    case "approval": return <ShieldAlert className="size-3.5 text-accent" />;
    case "question": return <MessageCircleQuestion className="size-3.5 text-accent" />;
    case "done": return <Check className="size-3.5 text-ok" />;
    case "error": return <X className="size-3.5 text-danger" />;
    case "denied": return <Ban className="size-3.5 text-faint" />;
  }
}

function ToolRow({ item, tabId, cwd, working }: { item: ToolItem; tabId: string; cwd: string; working: boolean }) {
  const [open, setOpen] = useState(false);
  const d = describe(item.name, item.input, item.status, cwd);
  const Icon = d.icon;
  const stale = !working && (item.status === "pending" || item.status === "running");
  if (item.status === "approval" && item.request) return <ApprovalCard item={item} tabId={tabId} />;
  if (item.status === "question" && item.question) return <QuestionCard item={item} tabId={tabId} />;
  return (
    <div className="text-[12.5px]">
      <button onClick={() => setOpen(!open)} className="group flex w-full min-w-0 items-center gap-2 rounded-md px-1 py-0.5 text-left hover:bg-hover/60">
        <Icon className="size-3.5 shrink-0 text-muted" />
        <span className={clsx("shrink-0", item.status === "denied" ? "text-faint line-through" : "text-fg")}>{d.verb}</span>
        <span className="min-w-0 truncate font-mono text-[12px] text-muted" title={str(item.input?.path) || undefined}>{d.target}</span>
        {d.plus !== undefined && item.status !== "denied" && (
          <span className="shrink-0 font-mono text-[11px]"><span className="text-ok">+{d.plus}</span> <span className="text-danger">−{d.minus}</span></span>
        )}
        <span className="ml-auto flex shrink-0 items-center gap-1.5">
          {item.status === "denied" && <span className="text-[11px] text-faint">declined</span>}
          {stale ? <X className="size-3.5 text-faint" /> : <StatusIcon status={item.status} />}
          <ChevronRight className={clsx("size-3 text-faint opacity-0 transition group-hover:opacity-100", open && "rotate-90 opacity-100")} />
        </span>
      </button>
      {open && <ToolDetails item={item} />}
    </div>
  );
}

/** What a step did: the diff of an edit, the file written, the command and its output. */
function ToolDetails({ item }: { item: ToolItem }) {
  const i = item.input ?? {};
  return (
    <div className="mt-1 ml-5 flex flex-col gap-1.5">
      {item.name === "edit" && <Diff before={str(i.old_string)} after={str(i.new_string)} />}
      {item.name === "write" && <Diff before="" after={str(i.content)} max={60} />}
      {item.name === "shell" && <Code>{`$ ${str(i.command)}`}</Code>}
      {item.output && !(item.name === "write" || item.name === "edit") && <Code muted={item.status !== "error"} danger={item.status === "error"}>{item.output}</Code>}
      {item.output && (item.name === "write" || item.name === "edit") && (item.status === "error" || item.status === "denied") && <Code danger>{item.output}</Code>}
    </div>
  );
}

function Code({ children, muted, danger }: { children: ReactNode; muted?: boolean; danger?: boolean }) {
  return (
    <pre className={clsx("max-h-72 overflow-auto rounded-md border border-line bg-panel px-2.5 py-1.5 font-mono text-[11.5px] leading-relaxed whitespace-pre-wrap",
      danger ? "text-danger" : muted ? "text-muted" : "text-fg")}>{children}</pre>
  );
}

function Diff({ before, after, max = 200 }: { before: string; after: string; max?: number }) {
  const rows = useMemo(() => {
    const del = before ? before.replace(/\n$/, "").split("\n").map((l) => ["-", l] as const) : [];
    const add = after ? after.replace(/\n$/, "").split("\n").map((l) => ["+", l] as const) : [];
    return [...del, ...add];
  }, [before, after]);
  const shown = rows.slice(0, max);
  return (
    <div className="max-h-80 overflow-auto rounded-md border border-line bg-panel py-1 font-mono text-[11.5px] leading-relaxed">
      {shown.map(([sign, text], n) => (
        <div key={n} className={clsx("flex whitespace-pre-wrap px-2", sign === "+" ? "bg-ok/10 text-ok" : "bg-danger/10 text-danger")}>
          <span className="w-4 shrink-0 select-none opacity-70">{sign}</span><span className="text-fg/90">{text || " "}</span>
        </div>
      ))}
      {rows.length > max && <div className="px-2 text-faint">… {rows.length - max} more lines</div>}
    </div>
  );
}

/** A tool call waiting for the user: what it will do, and Allow / Always allow / Deny. */
function ApprovalCard({ item, tabId }: { item: ToolItem; tabId: string }) {
  const req = item.request!;
  const i = item.input ?? {};
  const title = req.kind === "exec" ? "Run this command?" : req.kind === "fetch" ? "Fetch this page?" : req.kind === "edit-outside" ? "Edit a file outside the project?" : `${item.name === "write" ? "Write" : "Edit"} ${basename(str(i.path))}?`;
  const deny = () => void answer(tabId, item.id, { decision: "deny" });
  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") { e.preventDefault(); deny(); }
  };
  return (
    <div onKeyDown={onKey} className="rounded-lg border border-accent/40 bg-accent/5 p-3">
      <div className="mb-2 flex items-center gap-2 text-[13px] font-medium text-fg"><ShieldAlert className="size-4 text-accent" />{title}</div>
      {item.name === "shell" && <Code>{str(i.command)}</Code>}
      {item.name === "edit" && <><div className="mb-1 font-mono text-[11.5px] text-muted">{str(i.path)}</div><Diff before={str(i.old_string)} after={str(i.new_string)} /></>}
      {item.name === "write" && <><div className="mb-1 font-mono text-[11.5px] text-muted">{str(i.path)}</div><Diff before="" after={str(i.content)} max={40} /></>}
      {!["shell", "edit", "write"].includes(item.name) && <Code>{req.summary}</Code>}
      <div className="mt-2.5 flex flex-wrap items-center gap-2">
        <Button autoFocus size="sm" variant="primary" onClick={() => void answer(tabId, item.id, { decision: "allow" })}>Allow <kbd className="ml-1 opacity-60">↵</kbd></Button>
        <Button size="sm" onClick={() => void answer(tabId, item.id, { decision: "allow-for-session" })}>
          Always allow {item.name === "shell" ? "commands" : item.name === "web_fetch" ? "fetching pages" : item.name.startsWith("mcp__") ? "this tool" : "edits"} in this chat
        </Button>
        <Button size="sm" variant="ghost" onClick={deny}>Deny <kbd className="ml-1 opacity-60">Esc</kbd></Button>
      </div>
    </div>
  );
}

/** The model asks the user (ask_user): its options as buttons, or an answer in your own words. */
function QuestionCard({ item, tabId }: { item: ToolItem; tabId: string }) {
  const q = item.question!;
  const [own, setOwn] = useState("");
  const reply = (text: string) => void answerQuestion(tabId, item.id, text);
  return (
    <div className="rounded-lg border border-accent/40 bg-accent/5 p-3">
      <div className="mb-2.5 flex items-start gap-2 text-[13px] text-fg">
        <MessageCircleQuestion className="mt-0.5 size-4 shrink-0 text-accent" />
        <span className="whitespace-pre-wrap">{q.question}</span>
      </div>
      {q.options.length > 0 && (
        <div className="mb-2 flex flex-col gap-1.5">
          {q.options.map((o, n) => (
            <button key={n} onClick={() => reply(o)} autoFocus={n === 0}
              className="flex items-center gap-2.5 rounded-md border border-line bg-panel px-3 py-2 text-left text-[12.5px] text-fg hover:border-accent/60 hover:bg-hover">
              <span className="flex size-5 shrink-0 items-center justify-center rounded border border-line font-mono text-[10.5px] text-muted">{n + 1}</span>{o}
            </button>
          ))}
        </div>
      )}
      <form className="flex items-center gap-2" onSubmit={(e) => { e.preventDefault(); if (own.trim()) reply(own); }}>
        <input value={own} onChange={(e) => setOwn(e.target.value)} autoFocus={!q.options.length}
          placeholder={q.options.length ? "Or answer in your own words…" : "Your answer…"}
          className="h-8 min-w-0 flex-1 rounded-md border border-line bg-panel px-2.5 text-[12.5px] text-fg outline-none placeholder:text-faint focus:border-accent/60 focus-visible:outline-none" />
        <Button size="sm" variant="primary" type="submit" disabled={!own.trim()}>Answer</Button>
      </form>
    </div>
  );
}

/** Where the conversation was summarized: from here on the model works from the summary. */
function CompactedRow({ item, cwd }: { item: Extract<Item, { kind: "compacted" }>; cwd: string }) {
  const [open, setOpen] = useState(false);
  const label = item.state === "running" ? "Summarizing the conversation to free the context window…"
    : item.state === "failed" ? "Couldn't summarize the conversation"
    : `Conversation summarized${item.tokensBefore ? ` (${fmtTokens(item.tokensBefore)} tokens)` : ""}: the model continues from the summary`;
  return (
    <div className="flex flex-col gap-1.5">
      <button disabled={item.state !== "done"} onClick={() => setOpen(!open)} aria-expanded={open}
        className={clsx("flex items-center gap-2 text-[12px]", item.state === "failed" ? "text-danger" : "text-muted hover:text-fg")}>
        <span className="h-px flex-1 bg-line" />
        {item.state === "running" ? <Spinner className="size-3.5" /> : <FoldVertical className="size-3.5" />}
        <span>{label}</span>
        {item.state === "done" && <ChevronRight className={clsx("size-3 transition-transform", open && "rotate-90")} />}
        <span className="h-px flex-1 bg-line" />
      </button>
      {open && <div className="rounded-md border border-line bg-panel/60 px-3 py-2"><Markdown text={item.summary} cwd={cwd} className="text-[12.5px]" /></div>}
    </div>
  );
}

/** The agent's checklist (its latest `todo_write`), above the composer while there's work in it. */
function TodoPanel({ items, busy }: { items: Item[]; busy: boolean }) {
  const [open, setOpen] = useState(true);
  const todos = useMemo(() => {
    for (let i = items.length - 1; i >= 0; i--) {
      const it = items[i];
      if (it.kind === "user") return [];
      if (it.kind === "tool" && it.name === "todo_write" && it.status !== "denied") return todoList(it.input);
    }
    return [];
  }, [items]);
  if (!todos.length || (!busy && todos.every((t) => t.status === "completed"))) return null;
  const done = todos.filter((t) => t.status === "completed").length;
  const current = todos.find((t) => t.status === "in_progress");
  return (
    <div className="mx-auto mt-2 w-full max-w-[780px] px-5">
      <div className="rounded-xl border border-line bg-panel/70 text-[12px]">
        <button onClick={() => setOpen(!open)} aria-expanded={open} className="flex w-full min-w-0 items-center gap-2 px-3 py-1.5 text-left">
          <ListTodo className="size-3.5 shrink-0 text-muted" />
          <span className="shrink-0 text-fg">Plan</span>
          <span className="shrink-0 text-faint">{done}/{todos.length}</span>
          {!open && current && <span className="min-w-0 truncate text-muted">· {current.content}</span>}
          <ChevronDown className={clsx("ml-auto size-3 shrink-0 text-faint transition-transform", !open && "-rotate-90")} />
        </button>
        {open && (
          <ul className="flex max-h-40 flex-col gap-0.5 overflow-y-auto px-3 pb-2">
            {todos.map((t, n) => (
              <li key={n} className={clsx("flex items-start gap-2", t.status === "completed" ? "text-faint line-through" : t.status === "in_progress" ? "text-fg" : "text-muted")}>
                {t.status === "completed" ? <CircleCheck className="mt-px size-3.5 shrink-0 text-ok" /> : t.status === "in_progress" ? <CircleDot className="mt-px size-3.5 shrink-0 text-accent" /> : <Circle className="mt-px size-3.5 shrink-0" />}
                <span>{t.content}</span>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}

function TurnEnd({ item }: { item: Extract<Item, { kind: "end" }> }) {
  if (item.end === "error") {
    return <div className="rounded-md border border-danger/30 bg-danger/10 px-3 py-2 text-[12px] text-danger">{item.error ?? "Something went wrong."}</div>;
  }
  const parts = [
    item.end === "stopped" ? "Stopped" : item.end === "stepLimit" ? "Paused after many steps. Say “continue” to keep going" : null,
    item.elapsedMs ? `Took ${fmtDuration(item.elapsedMs)}` : null,
    item.tokens ? `${fmtTokens(item.tokens)} tokens` : null,
  ].filter(Boolean);
  if (!parts.length) return null;
  return <div className="border-b border-line/60 pb-3 text-[11px] text-faint">{parts.join(" · ")}</div>;
}

/** Files this chat changed, with a shortcut to the Diff view (like Zed's edits bar). */
function EditedFiles({ items, onReview }: { items: Item[]; onReview: () => void }) {
  const files = useMemo(() => {
    const m = new Map<string, { plus: number; minus: number }>();
    for (const it of items) {
      if (it.kind !== "tool" || it.status !== "done" || (it.name !== "edit" && it.name !== "write")) continue;
      const d = describe(it.name, it.input, it.status);
      const f = m.get(d.target) ?? { plus: 0, minus: 0 };
      m.set(d.target, { plus: f.plus + (d.plus ?? 0), minus: f.minus + (d.minus ?? 0) });
    }
    return [...m.entries()];
  }, [items]);
  if (!files.length) return null;
  const plus = files.reduce((n, [, f]) => n + f.plus, 0);
  const minus = files.reduce((n, [, f]) => n + f.minus, 0);
  return (
    <div className="mx-auto mt-2 w-full max-w-[780px] px-5">
      <div className="flex items-center gap-2 rounded-xl border border-line bg-panel/70 px-3 py-1.5 text-[12px]">
        <FilePen className="size-3.5 text-muted" />
        <span className="text-fg">{files.length} file{files.length === 1 ? "" : "s"} changed</span>
        <span className="font-mono text-[11px]"><span className="text-ok">+{plus}</span> <span className="text-danger">−{minus}</span></span>
        <span className="min-w-0 truncate text-faint" title={files.map(([p]) => p).join("\n")}>{files.map(([p]) => basename(p)).join(", ")}</span>
        <Button size="sm" variant="ghost" className="ml-auto" onClick={onReview}><GitCompare className="size-3.5" />Review</Button>
      </div>
    </div>
  );
}

/** How full the context window is (ring + %), with the details on click. */
function ContextMeter({ tabId, models }: { tabId: string; models: AgentModel[] }) {
  const chat = useAgent((s) => s.chats[tabId])!;
  const [open, setOpen] = useState(false);
  const win = contextWindow(chat.model, models);
  const used = chat.contextTokens;
  const pct = Math.min(100, Math.round((used / win.tokens) * 100));
  const tone = pct >= 85 ? "text-danger" : pct >= 60 ? "text-warn" : "text-muted";
  const r = 6, circ = 2 * Math.PI * r;
  const ctx = chat.context;
  const mcpTools = ctx?.mcp.reduce((n, m) => n + m.tools, 0) ?? 0;
  const u = chat.usage;
  const rate = u.input_tokens ? Math.round((u.cache_read_tokens / u.input_tokens) * 100) : 0;
  const Row = ({ label, children }: { label: string; children: ReactNode }) => (
    <div className="flex justify-between gap-6"><span className="text-muted">{label}</span><span className="text-right text-fg">{children}</span></div>
  );
  return (
    <span className="relative">
      <button onClick={() => setOpen(!open)} title="Context used: click for details" className={clsx("flex h-7 items-center gap-1.5 rounded-full px-2 hover:bg-hover", tone)}>
        <svg viewBox="0 0 16 16" className="size-3.5 -rotate-90">
          <circle cx="8" cy="8" r={r} fill="none" stroke="currentColor" strokeOpacity="0.25" strokeWidth="2.2" />
          <circle cx="8" cy="8" r={r} fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeDasharray={`${(circ * pct) / 100} ${circ}`} />
        </svg>
        {used ? `${pct}%` : "0%"}
      </button>
      {open && (
        <>
          <div className="fixed inset-0 z-20" onMouseDown={() => setOpen(false)} />
          <div className="absolute right-0 bottom-9 z-30 w-72 rounded-lg border border-line bg-panel p-3 text-[12px] shadow-2xl">
            <div className="mb-2 font-medium text-fg">Context</div>
            <div className="mb-1.5 h-1.5 overflow-hidden rounded-full bg-hover">
              <div className={clsx("h-full rounded-full", pct >= 85 ? "bg-danger" : pct >= 60 ? "bg-warn" : "bg-accent")} style={{ width: `${Math.max(pct, used ? 2 : 0)}%` }} />
            </div>
            <div className="mb-3 text-[11.5px] text-muted">
              {fmtTokens(used)} of {win.exact ? "" : "~"}{fmtTokens(win.tokens)} tokens used ({pct}%){win.exact ? "" : ". Window size is a guess for this model"}.
              {pct >= 85 && " Start a new session soon: the oldest messages will stop fitting."}
            </div>
            <div className="flex flex-col gap-1">
              <Row label="Model">{chat.model}</Row>
              <Row label="This session">{fmtTokens(u.input_tokens)} in · {fmtTokens(u.output_tokens)} out</Row>
              <Row label="From cache">{fmtTokens(u.cache_read_tokens)} ({rate}%)</Row>
              <Row label="Rules">{ctx?.rules.length ? <span title={ctx.rules.join("\n")}>{ctx.rules.length} file{ctx.rules.length === 1 ? "" : "s"}</span> : "none"}</Row>
              <Row label="Skills">{ctx?.skills.length ? <span title={ctx.skills.join("\n")}>{ctx.skills.length}</span> : "none"}</Row>
              <Row label="MCP">{ctx?.mcpLoading ? "connecting…" : ctx?.mcp.length ? <span title={ctx.mcp.map((m) => `${m.name}: ${m.tools} tools`).join("\n")}>{ctx.mcp.length} server{ctx.mcp.length === 1 ? "" : "s"} · {mcpTools} tools</span> : "none"}</Row>
            </div>
          </div>
        </>
      )}
    </span>
  );
}

function Composer({ tabId, visible, provider }: { tabId: string; visible: boolean; provider: AiProvider }) {
  const chat = useAgent((s) => s.chats[tabId])!;
  const [text, setText] = useState("");
  const [models, setModels] = useState<AgentModel[]>([]);
  useEffect(() => { void connectionModels(provider).then(setModels); }, [provider]);
  const box = useRef<HTMLTextAreaElement>(null);

  useEffect(() => { if (visible) box.current?.focus({ preventScroll: true }); }, [visible]);
  // grow with the text, up to about ten lines
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 240)}px`;
  }, [text]);

  const submit = async () => {
    if ((!text.trim() && !chat.attachments.length) || chat.busy) return;
    const t = text;
    setText("");
    if (!(await send(tabId, t))) setText(t);
  };
  const onKey = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      void submit();
    }
  };
  const tokens = chat.usage.input_tokens + chat.usage.output_tokens;
  const cacheRate = chat.usage.input_tokens ? Math.round((chat.usage.cache_read_tokens / chat.usage.input_tokens) * 100) : null;
  const policy = POLICIES.find((p) => p.id === chat.policy)!;
  const PolicyIcon = chat.policy === "bypass" ? ShieldOff : chat.policy === "auto-edit" ? ShieldCheck : Shield;
  return (
    <div className="mx-auto w-full max-w-[780px] px-5 pt-2 pb-3">
      <div className="rounded-2xl border border-line bg-panel shadow-[0_6px_24px_rgba(0,0,0,0.25)] transition-[border-color,box-shadow] focus-within:border-accent/60 focus-within:shadow-[0_0_0_3px_color-mix(in_srgb,var(--color-accent)_14%,transparent)]">
        {chat.attachments.length > 0 && (
          <div className="flex flex-wrap gap-1.5 px-3 pt-3">
            {chat.attachments.map((p) => <AttachmentChip key={p} path={p} onRemove={() => unattach(tabId, p)} />)}
          </div>
        )}
        <textarea
          ref={box}
          rows={2}
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={onKey}
          onPaste={(e) => {
            const files = [...e.clipboardData.files];
            if (files.length) { e.preventDefault(); void attach(tabId, files); }
          }}
          placeholder={chat.busy ? "Working… type your next message" : "Ask DevCat Agent to build, fix or explain something  ·  /compact to summarize the chat"}
          className="block max-h-[240px] min-h-[52px] w-full resize-none border-0 bg-transparent px-4 pt-3.5 pb-1 text-[13.5px] leading-relaxed text-fg outline-none placeholder:text-faint focus-visible:outline-none"
          // the composer's border shows focus; no second outline around the text box
          style={{ outline: "none", boxShadow: "none" }}
        />
        <div className="flex items-center gap-1.5 px-2.5 pt-1 pb-2.5">
          <button title="Attach files (or drop them here, or paste an image)" aria-label="Attach files"
            onClick={async () => {
              const picked = await pickFiles({ multiple: true, title: "Attach files" }).catch(() => null);
              const list = Array.isArray(picked) ? picked : picked ? [picked] : [];
              if (list.length) void attach(tabId, list.map(String));
            }}
            className="flex size-7 items-center justify-center rounded-full text-muted hover:bg-hover hover:text-fg"><Paperclip className="size-3.5" /></button>
          <label title={policy.hint} className={clsx("relative flex h-7 cursor-pointer items-center gap-1.5 rounded-full border px-2.5 text-[12px] transition-colors",
            chat.policy === "bypass" ? "border-warn/40 bg-warn/10 text-warn" : "border-line text-muted hover:border-faint hover:text-fg")}>
            <PolicyIcon className="size-3.5" />
            {policy.label}
            <ChevronDown className="size-3 opacity-60" />
            <select value={chat.policy} onChange={(e) => setPolicy(tabId, e.target.value as AgentPolicy)} className="absolute inset-0 cursor-pointer opacity-0" aria-label="Permission mode">
              {POLICIES.map((p) => <option key={p.id} value={p.id}>{p.label}: {p.hint}</option>)}
            </select>
          </label>
          <label className="relative flex h-7 max-w-[240px] cursor-pointer items-center gap-1.5 rounded-full bg-hover/70 px-2.5 text-[12px] text-muted hover:text-fg"
            title={models.length ? `Model for the next messages (${chat.providerName})` : `${chat.providerName}: ${chat.model}`}>
            <Cpu className="size-3.5 shrink-0" /><span className="truncate">{chat.model}</span>
            {models.length > 0 && <ChevronDown className="size-3 shrink-0 opacity-60" />}
            {models.length > 0 && (
              <select value={chat.model} onChange={(e) => setModel(tabId, e.target.value)} className="absolute inset-0 cursor-pointer opacity-0" aria-label="Model">
                {!models.some((m) => m.id === chat.model) && <option value={chat.model}>{chat.model}</option>}
                {models.map((m) => <option key={m.id} value={m.id}>{m.id}{m.context ? ` · ${fmtTokens(m.context)}` : ""}</option>)}
              </select>
            )}
          </label>
          <span className="ml-auto flex items-center gap-2 text-[11px] text-faint">
            <ContextMeter tabId={tabId} models={models} />
            {tokens > 0 && cacheRate !== null && cacheRate > 0 && <span title="Share of this chat's input the provider read from its cache">{cacheRate}% cached</span>}
          </span>
          {chat.busy ? (
            <button onClick={() => stop(tabId)} title="Stop" className="flex size-8 items-center justify-center rounded-full bg-fg text-app transition hover:opacity-85"><Square className="size-3 fill-current" /></button>
          ) : (
            <button onClick={() => void submit()} disabled={!text.trim() && !chat.attachments.length} title="Send (Enter) · new line: Shift+Enter"
              className="flex size-8 items-center justify-center rounded-full bg-accent text-accent-fg transition hover:brightness-110 disabled:bg-hover disabled:text-faint"><ArrowUp className="size-4" /></button>
          )}
        </div>
      </div>
    </div>
  );
}
