// DevCat Agent chat on the phone: the same conversation as the desktop chat tab (steps folded into
// one line, approvals and questions as cards, reasoning folded away), with the composer docked at
// the bottom. The chat runs on the computer; this screen opens it and follows its `agent-event`s.
// Turning the session into items is shared with the desktop (src/features/agent/model.ts).
import clsx from "clsx";
import {
  ArrowUp, Ban, BookOpen, Bot, Brain, Check, ChevronDown, ChevronRight, Cpu, File as FileIcon, FilePen, FolderOpen, Layers, MessageCircleQuestion, MoreVertical, Paperclip, Plug,
  Shield, ShieldAlert, ShieldCheck, ShieldOff, Square, TriangleAlert, X,
} from "lucide-react";
import { memo, useDeferredValue, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  POLICIES, SUGGESTIONS, ZERO, applyAll, basename, contextWindow, describe, diffRows, editedFiles, fmtDuration, fmtTokens, itemsFromLog, lastContext, str, summarize, toBlocks,
  type ChatView, type Item, type ToolItem, type ToolStatus,
} from "../../../src/features/agent/model";
import type { AgentDecision, AgentLiveEvent, AgentModel, AgentOpened, AgentPolicy } from "../../../src/lib/types";
import { errMsg, onEvent } from "../lib/rpc";
import { host, nav, toastErr, useApp } from "../lib/store";
import { Btn, Confirm, Empty, ErrorBox, Item as Row, Loading, Screen, Sheet, Spinner, folderName } from "../ui";
import { Markdown } from "./files";

interface Chat extends ChatView {
  sessionId: string;
  model: string;
  providerName: string;
}

const KEY_POLICY = "devcat.mobile.agentPolicy";
function savedPolicy(): AgentPolicy {
  try {
    const p = localStorage.getItem(KEY_POLICY);
    return p === "auto-edit" || p === "bypass" ? p : "ask";
  } catch {
    return "ask";
  }
}

function toBase64(f: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(String(r.result).replace(/^data:[^,]*,/, ""));
    r.onerror = () => reject(r.error);
    r.readAsDataURL(f);
  });
}

/** Open the chat on the computer and follow it: reopened when the app comes back or reconnects. */
function useChat(cwd: string, providerId: string, initial: string | null) {
  const [chat, setChat] = useState<Chat | null>(null);
  const [error, setError] = useState<string | null>(null);
  const id = useRef(initial);
  const online = useApp((s) => s.conn[s.current ?? ""]?.state === "online");

  useEffect(() => {
    if (!online) return;
    let disposed = false;
    // Events that arrive before the chat is open wait for it; ones it already has (by `seq`) are skipped.
    let opened: { seq: number } | null = null;
    let early: { event: AgentLiveEvent; seq?: number }[] = [];
    let queued: AgentLiveEvent[] = [];
    let frame = 0;
    const flush = () => {
      frame = 0;
      const batch = queued;
      queued = [];
      setChat((c) => (c ? applyAll(c, batch) : c));
    };
    const off = onEvent<{ sessionId: string; event: AgentLiveEvent; seq?: number }>("agent-event", (p) => {
      if (p.sessionId !== id.current && id.current) return;
      if (!opened) { early.push(p); return; }
      if (p.seq !== undefined && p.seq <= opened.seq) return;
      queued.push(p.event);
      // apply once per frame (streamed text comes in many small pieces); hidden, apply at once
      if (document.hidden) flush();
      else frame ||= requestAnimationFrame(flush);
    });
    const open = async () => {
      try {
        const o = await host<AgentOpened>("agent_open", { cwd, providerId, sessionId: id.current });
        if (disposed) return;
        id.current = o.sessionId;
        const seq = o.seq ?? 0;
        const later = early.filter((e) => e.seq === undefined || e.seq > seq).map((e) => e.event);
        early = [];
        opened = { seq };
        const base: Chat = {
          sessionId: o.sessionId, title: o.title, model: o.model, providerName: o.providerName, busy: o.busy, error: null,
          items: itemsFromLog(o.events), usage: o.usage ?? ZERO, context: o.context, contextTokens: lastContext(o.events),
        };
        setChat((prev) => ({ ...applyAll(base, [...(o.live ?? []), ...later]), model: prev?.sessionId === o.sessionId ? prev.model : o.model }));
        setError(null);
      } catch (e) {
        if (!disposed) setError(errMsg(e));
      }
    };
    void open();
    const onVis = () => {
      if (document.hidden) return;
      opened = null;
      void open();
    };
    document.addEventListener("visibilitychange", onVis);
    return () => {
      disposed = true;
      off();
      if (frame) cancelAnimationFrame(frame);
      document.removeEventListener("visibilitychange", onVis);
      if (id.current) void host("agent_detach", { sessionId: id.current }).catch(() => {});
    };
  }, [cwd, providerId, online]);

  return { chat, setChat, error, setError };
}

export function AgentChat({ cwd, providerId, sessionId, first }: { cwd: string; providerId: string; sessionId: string | null; first?: string }) {
  const { chat, setChat, error, setError } = useChat(cwd, providerId, sessionId);
  const readonly = useApp((s) => s.machines.find((m) => m.id === s.current)?.access === "readonly");
  const info = useApp((s) => s.chats.find((c) => c.sessionId === chat?.sessionId));
  const [policy, setPolicyState] = useState(savedPolicy);
  const [models, setModels] = useState<AgentModel[]>([]);
  const [menu, setMenu] = useState(false);
  const [end, setEnd] = useState(false);
  const [attachments, setAttachments] = useState<string[]>([]);
  const sent = useRef(false);

  useEffect(() => { void host<AgentModel[]>("agent_models", { providerId: info?.providerId || providerId }).then(setModels).catch(() => {}); }, [providerId, info?.providerId]);

  const setPolicy = (p: AgentPolicy) => {
    try { localStorage.setItem(KEY_POLICY, p); } catch { /* private mode */ }
    setPolicyState(p);
  };

  const send = async (text: string): Promise<boolean> => {
    if (!chat || chat.busy || (!text.trim() && !attachments.length)) return false;
    try {
      // the window decides when the conversation gets summarized
      await host("agent_send", { sessionId: chat.sessionId, text: text.trim(), policy, model: chat.model || null, attachments, contextWindow: contextWindow(chat.model, models).tokens });
      setAttachments([]);
      return true;
    } catch (e) {
      setError(errMsg(e));
      return false;
    }
  };
  // A first message from New chat: sent once the chat is open.
  useEffect(() => {
    if (chat && first && !sent.current) {
      sent.current = true;
      void send(first);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [chat?.sessionId]);

  const decide = (toolUseId: string, decision: AgentDecision) => {
    if (!chat) return;
    setChat((c) => c && { ...c, items: c.items.map((i) => (i.kind === "tool" && i.id === toolUseId ? { ...i, request: null, status: decision.decision === "deny" ? "denied" : "running" } : i)) });
    void host("agent_approve", { sessionId: chat.sessionId, toolUseId, decision }).catch((e) => setError(errMsg(e)));
  };
  const reply = (toolUseId: string, text: string) => {
    if (!chat || !text.trim()) return;
    setChat((c) => c && { ...c, items: c.items.map((i) => (i.kind === "tool" && i.id === toolUseId ? { ...i, status: "running", question: null } : i)) });
    void host("agent_answer", { sessionId: chat.sessionId, toolUseId, answer: text.trim() }).catch((e) => setError(errMsg(e)));
  };
  const stop = () => { if (chat) void host("agent_stop", { sessionId: chat.sessionId }).catch(toastErr); };

  const waiting = chat?.items.some((i) => i.kind === "tool" && ((i.status === "approval" && i.request) || (i.status === "question" && i.question)));
  const sub = !chat ? "DevCat Agent" : waiting ? <span className="text-warn">⚠ needs you</span> : chat.busy ? <span className="text-warn">◐ working</span> : chat.title || `${chat.providerName} · ${chat.model}`;
  const fromPhone = info?.origin.startsWith("phone:") ?? !sessionId;

  return (
    <Screen title={folderName(cwd) || "Chat"} sub={sub} scroll={false} className="flex flex-col"
      right={chat && <button aria-label="Chat menu" className="flex size-9 items-center justify-center text-muted" onClick={() => setMenu(true)}><MoreVertical className="size-5" /></button>}>
      {!chat ? (
        error ? <div className="p-3"><Empty icon={<Bot className="size-8" />} title="DevCat Agent couldn't start"><span className="text-danger">{error}</span></Empty></div> : <Loading text="Opening the chat…" />
      ) : (
        <>
          <Conversation chat={chat} cwd={cwd} readonly={readonly} policy={policy} onSend={(t) => void send(t)} onDecide={decide} onReply={reply} />
          {error && (
            <div className="px-3 pb-1">
              <div className="flex items-start gap-2 rounded-xl border border-danger/30 bg-danger/10 px-3 py-2 text-[12.5px] text-danger">
                <span className="selectable min-w-0 flex-1 break-words">{error}</span>
                <button aria-label="Dismiss" onClick={() => setError(null)}><X className="size-4" /></button>
              </div>
            </div>
          )}
          <EditedFiles items={chat.items} cwd={cwd} />
          {readonly ? (
            <div className="mx-3 mb-2 rounded-lg bg-hover px-3 py-2 text-center text-[12px] text-muted" style={{ marginBottom: "max(8px, env(safe-area-inset-bottom))" }}>Read-only access: you can watch but not reply.</div>
          ) : (
            <Composer chat={chat} models={models} policy={policy} onPolicy={setPolicy} onModel={(m) => setChat((c) => c && { ...c, model: m })}
              attachments={attachments} setAttachments={setAttachments} onSend={send} onStop={stop} onError={setError} />
          )}
        </>
      )}
      <Sheet open={menu} onClose={() => setMenu(false)} title={folderName(cwd)}>
        {chat && <ContextDetails chat={chat} models={models} />}
        <Row onClick={() => { setMenu(false); nav.push({ s: "files", dir: cwd, root: cwd }); }} icon={<FolderOpen className="size-4" />} title="Browse files" chevron />
        {!readonly && chat?.busy && <Row onClick={() => { setMenu(false); stop(); }} icon={<Square className="size-4" />} title="Stop the agent" />}
        {!readonly && fromPhone && <Row onClick={() => { setMenu(false); setEnd(true); }} icon={<X className="size-4 text-danger" />} title={<span className="text-danger">End chat</span>} />}
      </Sheet>
      <Confirm open={end} danger title="End this chat?" confirm="End chat" onCancel={() => setEnd(false)}
        onConfirm={() => { setEnd(false); if (chat) void host("agent_close", { sessionId: chat.sessionId }).then(() => nav.back()).catch(toastErr); }}>
        It stops on the computer. The conversation is kept: resume it from Session history.
      </Confirm>
    </Screen>
  );
}

// ------------------------------------------------------------------ conversation

function Conversation({ chat, cwd, readonly, policy, onSend, onDecide, onReply }: {
  chat: Chat; cwd: string; readonly: boolean; policy: AgentPolicy; onSend: (t: string) => void; onDecide: (id: string, d: AgentDecision) => void; onReply: (id: string, t: string) => void;
}) {
  const scroller = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
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
  const ctx: ItemCtx = { cwd, readonly, onDecide, onReply };
  return (
    <div ref={scroller} onScroll={onScroll} className="min-h-0 flex-1 overflow-y-auto overscroll-contain">
      <div className="flex flex-col gap-3 px-3.5 pt-4 pb-3">
        {chat.items.length === 0 ? (
          <Welcome chat={chat} cwd={cwd} policy={policy} onSend={readonly ? undefined : onSend} />
        ) : (
          blocks.map((b) => b.kind === "steps"
            ? <Steps key={b.items[0].key} items={b.items} ctx={ctx} live={chat.busy && b.last === chat.items.length - 1} working={chat.busy && b.last > lastUser} />
            : <ItemView key={b.item.key} item={b.item} ctx={ctx} working={chat.busy && b.n > lastUser} />)
        )}
        {chat.busy && blocks[blocks.length - 1]?.kind !== "steps" && <Working items={chat.items} />}
      </div>
    </div>
  );
}

type ItemCtx = { cwd: string; readonly: boolean; onDecide: (id: string, d: AgentDecision) => void; onReply: (id: string, t: string) => void };

function Welcome({ chat, cwd, policy, onSend }: { chat: Chat; cwd: string; policy: AgentPolicy; onSend?: (t: string) => void }) {
  const p = POLICIES.find((x) => x.id === policy)!;
  return (
    <div className="flex flex-col items-center gap-4 pt-[8vh] text-center">
      <div className="flex size-11 items-center justify-center rounded-xl bg-accent/15 text-accent"><Bot className="size-6" /></div>
      <div>
        <h2 className="text-[17px] font-semibold">What should we do in {basename(cwd)}?</h2>
        <p className="mt-1 text-[12px] text-muted">{chat.providerName} · {chat.model} · {p.hint.toLowerCase()}.</p>
      </div>
      <ContextLine chat={chat} />
      {onSend && (
        <div className="flex flex-col items-stretch gap-2 self-stretch">
          {SUGGESTIONS.map((s) => (
            <button key={s} onClick={() => onSend(s)} className="rounded-xl border border-line px-3 py-2.5 text-[13px] text-muted active:bg-hover">{s}</button>
          ))}
        </div>
      )}
    </div>
  );
}

function ContextLine({ chat }: { chat: Chat }) {
  const ctx = chat.context;
  if (!ctx) return null;
  const parts: ReactNode[] = [];
  if (ctx.rules.length) parts.push(<span key="r" className="flex items-center gap-1"><BookOpen className="size-3" />{ctx.rules.length === 1 ? ctx.rules[0] : `${ctx.rules.length} rule files`}</span>);
  if (ctx.skills.length) parts.push(<span key="s">{ctx.skills.length} skill{ctx.skills.length === 1 ? "" : "s"}</span>);
  for (const m of ctx.mcp) parts.push(<span key={`m${m.name}`} className="flex items-center gap-1"><Plug className="size-3" />{m.name} ({m.tools} tool{m.tools === 1 ? "" : "s"})</span>);
  if (ctx.mcpLoading) parts.push(<span key="ml" className="flex items-center gap-1"><Spinner className="size-3" />connecting MCP servers…</span>);
  return (
    <div className="flex flex-col items-center gap-1 text-[11.5px] text-faint">
      {parts.length > 0 && <div className="flex flex-wrap items-center justify-center gap-x-3 gap-y-1"><span>Using</span>{parts}</div>}
      {ctx.notices.map((n) => <div key={n} className="flex items-start gap-1.5 text-warn"><TriangleAlert className="mt-0.5 size-3 shrink-0" />{n}</div>)}
    </div>
  );
}

/** A run of steps folded into one line; the step in progress shows under it while it runs. */
function Steps({ items, ctx, live, working }: { items: Item[]; ctx: ItemCtx; live: boolean; working: boolean }) {
  const [open, setOpen] = useState(false);
  const s = summarize(items);
  const current = live ? items[items.length - 1] : null;
  return (
    <div className="flex flex-col gap-1">
      <button onClick={() => setOpen(!open)} aria-expanded={open} className="flex w-full min-w-0 items-center gap-2 rounded-lg px-1 py-1 text-left text-[13px] active:bg-hover">
        {live ? <Spinner className="size-3.5 shrink-0" /> : <Layers className="size-3.5 shrink-0 text-muted" />}
        <span className="shrink-0">{live ? "Working" : s.explored ? "Explored" : "Worked"}</span>
        <span className="min-w-0 truncate text-muted">{s.text}</span>
        {s.failed > 0 && <span className="shrink-0 text-[12px] text-danger">· {s.failed} failed</span>}
        <ChevronRight className={clsx("ml-auto size-3.5 shrink-0 text-faint transition", open && "rotate-90")} />
      </button>
      {open ? (
        <div className="ml-[7px] flex flex-col gap-1 border-l border-line pl-3">
          {items.map((it) => <ItemView key={it.key} item={it} ctx={ctx} working={working} />)}
        </div>
      ) : current && (
        <div className="ml-[7px] border-l border-line pl-3"><ItemView key={current.key} item={current} ctx={ctx} working={working} /></div>
      )}
    </div>
  );
}

function Working({ items }: { items: Item[] }) {
  const last = items[items.length - 1];
  const quiet = !last || last.kind === "user" || (last.kind === "tool" && (last.status === "done" || last.status === "error" || last.status === "denied"));
  if (!quiet) return null;
  return <div className="flex items-center gap-2 text-[12.5px] text-muted"><Spinner className="size-3.5" />Working…</div>;
}

const ItemView = memo(function ItemView({ item, ctx, working }: { item: Item; ctx: ItemCtx; working: boolean }) {
  switch (item.kind) {
    case "user":
      return (
        <div className="flex flex-col items-end gap-1.5">
          {item.attachments.length > 0 && <div className="flex max-w-[88%] flex-wrap justify-end gap-1.5">{item.attachments.map((p) => <Chip key={p} path={p} />)}</div>}
          {item.text && <div className="selectable max-w-[88%] rounded-2xl rounded-br-md bg-raised px-3.5 py-2 text-[14px] break-words whitespace-pre-wrap">{item.text}</div>}
        </div>
      );
    case "answer":
      return <Answer item={item} />;
    case "tool":
      if (item.status === "approval" && item.request) return <ApprovalCard item={item} ctx={ctx} />;
      if (item.status === "question" && item.question) return <QuestionCard item={item} ctx={ctx} />;
      return <ToolRow item={item} cwd={ctx.cwd} working={working} />;
    case "end":
      return <TurnEnd item={item} />;
    case "compacted":
      return (
        <div className="flex items-center gap-2 text-[12px] text-muted">
          <span className="h-px flex-1 bg-line" />
          {item.state === "running" ? <><Spinner className="size-3.5" />Summarizing the conversation…</>
            : item.state === "failed" ? <span className="text-danger">Couldn't summarize the conversation</span>
            : <>Conversation summarized{item.tokensBefore ? ` (${fmtTokens(item.tokensBefore)} tokens)` : ""}</>}
          <span className="h-px flex-1 bg-line" />
        </div>
      );
  }
});

function Chip({ path, onRemove }: { path: string; onRemove?: () => void }) {
  return (
    <span className="flex h-7 max-w-[220px] items-center gap-1.5 rounded-lg border border-line bg-panel px-2 text-[12px] text-muted">
      <FileIcon className="size-3.5 shrink-0" /><span className="truncate">{basename(path)}</span>
      {onRemove && <button onClick={onRemove} className="-mr-1 p-1 text-faint" aria-label={`Remove ${basename(path)}`}><X className="size-3" /></button>}
    </span>
  );
}

function Answer({ item }: { item: Extract<Item, { kind: "answer" }> }) {
  const [choice, setChoice] = useState<boolean | null>(null);
  const thinkingLive = item.streaming && !item.text;
  const open = choice ?? thinkingLive;
  const text = useDeferredValue(item.text);
  const thinking = useDeferredValue(item.thinking);
  return (
    <div className="flex flex-col gap-1.5">
      {item.thinking.trim() && (
        <div>
          <button className="flex items-center gap-1.5 py-0.5 text-[12.5px] text-muted" onClick={() => setChoice(!open)} aria-expanded={open}>
            <Brain className="size-3.5" />
            {thinkingLive ? <span className="animate-pulse">Thinking…</span> : item.thinkingMs ? `Thought for ${fmtDuration(item.thinkingMs)}` : "Thought"}
            <ChevronRight className={clsx("size-3 transition-transform", open && "rotate-90")} />
          </button>
          {open && <div className="selectable mt-1.5 max-h-64 overflow-y-auto border-l-2 border-line pl-3 text-[12.5px] leading-relaxed whitespace-pre-wrap text-muted">{thinking.trim()}</div>}
        </div>
      )}
      {text.trim() && <Markdown text={text} className="text-[14px]" />}
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

function ToolRow({ item, cwd, working }: { item: ToolItem; cwd: string; working: boolean }) {
  const [open, setOpen] = useState(false);
  const d = describe(item.name, item.input, item.status, cwd);
  const Icon = d.icon;
  const stale = !working && (item.status === "pending" || item.status === "running");
  return (
    <div className="text-[13px]">
      <button onClick={() => setOpen(!open)} className="flex w-full min-w-0 items-center gap-2 rounded-lg px-1 py-1 text-left active:bg-hover">
        <Icon className="size-3.5 shrink-0 text-muted" />
        <span className={clsx("shrink-0", item.status === "denied" && "text-faint line-through")}>{d.verb}</span>
        <span className="min-w-0 truncate font-mono text-[12px] text-muted">{d.target}</span>
        {d.plus !== undefined && item.status !== "denied" && <span className="shrink-0 font-mono text-[11px]"><span className="text-ok">+{d.plus}</span> <span className="text-danger">−{d.minus}</span></span>}
        <span className="ml-auto flex shrink-0 items-center gap-1.5">
          {item.status === "denied" && <span className="text-[11px] text-faint">declined</span>}
          {stale ? <X className="size-3.5 text-faint" /> : <StatusIcon status={item.status} />}
        </span>
      </button>
      {open && <ToolDetails item={item} />}
    </div>
  );
}

function ToolDetails({ item }: { item: ToolItem }) {
  const i = item.input ?? {};
  const file = item.name === "write" || item.name === "edit";
  return (
    <div className="mt-1 ml-5 flex flex-col gap-1.5">
      {item.name === "edit" && <Diff before={str(i.old_string)} after={str(i.new_string)} />}
      {item.name === "write" && <Diff before="" after={str(i.content)} max={60} />}
      {item.name === "shell" && <Code>{`$ ${str(i.command)}`}</Code>}
      {item.output && !file && <Code tone={item.status === "error" ? "danger" : "muted"}>{item.output}</Code>}
      {item.output && file && (item.status === "error" || item.status === "denied") && <Code tone="danger">{item.output}</Code>}
    </div>
  );
}

function Code({ children, tone }: { children: ReactNode; tone?: "muted" | "danger" }) {
  return (
    <pre className={clsx("selectable max-h-72 overflow-auto rounded-lg border border-line bg-panel px-2.5 py-1.5 font-mono text-[11.5px] leading-relaxed break-words whitespace-pre-wrap",
      tone === "danger" ? "text-danger" : tone === "muted" ? "text-muted" : "text-fg")}>{children}</pre>
  );
}

function Diff({ before, after, max = 200 }: { before: string; after: string; max?: number }) {
  const rows = useMemo(() => diffRows(before, after), [before, after]);
  return (
    <div className="selectable max-h-80 overflow-auto rounded-lg border border-line bg-panel py-1 font-mono text-[11.5px] leading-relaxed">
      {rows.slice(0, max).map(([sign, text], n) => (
        <div key={n} className={clsx("flex px-2 break-all whitespace-pre-wrap", sign === "+" ? "bg-ok/10 text-ok" : "bg-danger/10 text-danger")}>
          <span className="w-4 shrink-0 opacity-70">{sign}</span><span className="text-fg/90">{text || " "}</span>
        </div>
      ))}
      {rows.length > max && <div className="px-2 text-faint">… {rows.length - max} more lines</div>}
    </div>
  );
}

/** A tool call waiting for the user: what it will do, and Allow / Always allow / Deny. */
function ApprovalCard({ item, ctx }: { item: ToolItem; ctx: ItemCtx }) {
  const req = item.request!;
  const i = item.input ?? {};
  const title = item.name === "db_query" ? (req.kind === "query" ? `Read from ${str(i.connection)}?` : `Run this on ${str(i.connection)}? It may change data`)
    : req.kind === "query" ? `Read from ${str(i.connection)}?`
    : req.kind === "exec" ? "Run this command?" : req.kind === "fetch" ? "Fetch this page?" : req.kind === "edit-outside" ? "Edit a file outside the project?" : `${item.name === "write" ? "Write" : "Edit"} ${basename(str(i.path))}?`;
  return (
    <div className="rounded-xl border border-accent/40 bg-accent/5 p-3">
      <div className="mb-2 flex items-center gap-2 text-[14px] font-semibold"><ShieldAlert className="size-4 text-accent" />{title}</div>
      {item.name === "shell" && <Code>{str(i.command)}</Code>}
      {item.name === "edit" && <><div className="mb-1 font-mono text-[11.5px] break-all text-muted">{str(i.path)}</div><Diff before={str(i.old_string)} after={str(i.new_string)} /></>}
      {item.name === "write" && <><div className="mb-1 font-mono text-[11.5px] break-all text-muted">{str(i.path)}</div><Diff before="" after={str(i.content)} max={40} /></>}
      {item.name === "db_query" && <Code>{str(i.query)}</Code>}
      {!["shell", "edit", "write", "db_query"].includes(item.name) && <Code>{req.summary}</Code>}
      {ctx.readonly ? (
        <div className="mt-2 text-[12px] text-muted">Waiting for an answer on the computer or a phone with full access.</div>
      ) : (
        <div className="mt-2.5 flex flex-col gap-2">
          <Btn variant="primary" block onClick={() => ctx.onDecide(item.id, { decision: "allow" })}>Allow</Btn>
          <Btn block onClick={() => ctx.onDecide(item.id, { decision: "allow-for-session" })}>Always allow {item.name === "shell" ? "commands" : item.name === "web_fetch" ? "fetching pages" : item.name === "db_query" ? "queries" : item.name.startsWith("k8s_") ? "cluster reads" : item.name.startsWith("mcp__") ? "this tool" : "edits"} in this chat</Btn>
          <Btn variant="ghost" block onClick={() => ctx.onDecide(item.id, { decision: "deny" })}>Deny</Btn>
        </div>
      )}
    </div>
  );
}

/** The model asks the user (ask_user): its options as buttons, or an answer in your own words. */
function QuestionCard({ item, ctx }: { item: ToolItem; ctx: ItemCtx }) {
  const q = item.question!;
  const [own, setOwn] = useState("");
  return (
    <div className="rounded-xl border border-accent/40 bg-accent/5 p-3">
      <div className="mb-2.5 flex items-start gap-2 text-[14px]">
        <MessageCircleQuestion className="mt-0.5 size-4 shrink-0 text-accent" />
        <span className="selectable whitespace-pre-wrap">{q.question}</span>
      </div>
      {!ctx.readonly && (
        <>
          {q.options.length > 0 && (
            <div className="mb-2 flex flex-col gap-1.5">
              {q.options.map((o, n) => (
                <button key={n} onClick={() => ctx.onReply(item.id, o)} className="flex items-center gap-2.5 rounded-xl border border-line bg-panel px-3 py-2.5 text-left text-[13.5px] active:bg-hover">
                  <span className="flex size-6 shrink-0 items-center justify-center rounded-full bg-hover font-mono text-[12px] font-bold">{n + 1}</span>{o}
                </button>
              ))}
            </div>
          )}
          <form className="flex items-center gap-2" onSubmit={(e) => { e.preventDefault(); if (own.trim()) ctx.onReply(item.id, own); }}>
            <input value={own} onChange={(e) => setOwn(e.target.value)} placeholder={q.options.length ? "Or answer in your own words…" : "Your answer…"}
              className="h-10 min-w-0 flex-1 rounded-xl border border-line bg-panel px-3 text-[14px] outline-none placeholder:text-faint focus:border-accent/60" />
            <Btn variant="primary" type="submit" disabled={!own.trim()} className="h-10">Answer</Btn>
          </form>
        </>
      )}
    </div>
  );
}

function TurnEnd({ item }: { item: Extract<Item, { kind: "end" }> }) {
  if (item.end === "error") return <ErrorBox error={item.error ?? "Something went wrong."} />;
  const parts = [
    item.end === "stopped" ? "Stopped" : item.end === "stepLimit" ? "Paused after many steps. Say “continue” to keep going" : null,
    item.elapsedMs ? `Took ${fmtDuration(item.elapsedMs)}` : null,
    item.tokens ? `${fmtTokens(item.tokens)} tokens` : null,
  ].filter(Boolean);
  if (!parts.length) return null;
  return <div className="border-b border-line/60 pb-3 text-[11.5px] text-faint">{parts.join(" · ")}</div>;
}

/** Files this chat changed; tap to browse the project. */
function EditedFiles({ items, cwd }: { items: Item[]; cwd: string }) {
  const files = useMemo(() => editedFiles(items), [items]);
  if (!files.length) return null;
  const plus = files.reduce((n, [, f]) => n + f.plus, 0);
  const minus = files.reduce((n, [, f]) => n + f.minus, 0);
  return (
    <div className="px-3 pt-1">
      <button onClick={() => nav.push({ s: "files", dir: cwd, root: cwd })} className="flex w-full items-center gap-2 rounded-xl border border-line bg-panel/70 px-3 py-2 text-left text-[12.5px] active:bg-hover">
        <FilePen className="size-3.5 shrink-0 text-muted" />
        <span className="shrink-0">{files.length} file{files.length === 1 ? "" : "s"} changed</span>
        <span className="shrink-0 font-mono text-[11px]"><span className="text-ok">+{plus}</span> <span className="text-danger">−{minus}</span></span>
        <span className="min-w-0 truncate text-faint">{files.map(([p]) => basename(p)).join(", ")}</span>
        <ChevronRight className="ml-auto size-3.5 shrink-0 text-faint" />
      </button>
    </div>
  );
}

// ------------------------------------------------------------------ composer

function usedPct(chat: Chat, models: AgentModel[]) {
  const win = contextWindow(chat.model, models);
  return { win, pct: Math.min(100, Math.round((chat.contextTokens / win.tokens) * 100)) };
}

/** Context and token use (the chat menu). */
function ContextDetails({ chat, models }: { chat: Chat; models: AgentModel[] }) {
  const { win, pct } = usedPct(chat, models);
  const u = chat.usage;
  const rate = u.input_tokens ? Math.round((u.cache_read_tokens / u.input_tokens) * 100) : 0;
  const ctx = chat.context;
  const R = ({ label, children }: { label: string; children: ReactNode }) => <div className="flex justify-between gap-4"><span className="text-muted">{label}</span><span className="min-w-0 truncate text-right">{children}</span></div>;
  return (
    <div className="space-y-1.5 rounded-xl bg-raised px-3 py-2.5 text-[12.5px]">
      <div className="h-1.5 overflow-hidden rounded-full bg-hover">
        <div className={clsx("h-full rounded-full", pct >= 85 ? "bg-danger" : pct >= 60 ? "bg-warn" : "bg-accent")} style={{ width: `${Math.max(pct, chat.contextTokens ? 2 : 0)}%` }} />
      </div>
      <div className="text-[11.5px] text-muted">
        {fmtTokens(chat.contextTokens)} of {win.exact ? "" : "~"}{fmtTokens(win.tokens)} tokens of context used ({pct}%).{pct >= 85 && " Start a new chat soon: the oldest messages will stop fitting."}
      </div>
      <R label="Model">{chat.model}</R>
      <R label="This chat">{fmtTokens(u.input_tokens)} in · {fmtTokens(u.output_tokens)} out</R>
      <R label="From cache">{fmtTokens(u.cache_read_tokens)} ({rate}%)</R>
      <R label="Rules">{ctx?.rules.length ? `${ctx.rules.length} file${ctx.rules.length === 1 ? "" : "s"}` : "none"}</R>
      <R label="Skills">{ctx?.skills.length || "none"}</R>
      <R label="MCP">{ctx?.mcpLoading ? "connecting…" : ctx?.mcp.length ? `${ctx.mcp.length} server${ctx.mcp.length === 1 ? "" : "s"}` : "none"}</R>
    </div>
  );
}

function Composer({ chat, models, policy, onPolicy, onModel, attachments, setAttachments, onSend, onStop, onError }: {
  chat: Chat; models: AgentModel[]; policy: AgentPolicy; onPolicy: (p: AgentPolicy) => void; onModel: (m: string) => void;
  attachments: string[]; setAttachments: (f: (a: string[]) => string[]) => void; onSend: (t: string) => Promise<boolean>; onStop: () => void; onError: (e: string) => void;
}) {
  const [text, setText] = useState("");
  const [uploading, setUploading] = useState(0);
  const box = useRef<HTMLTextAreaElement>(null);
  const picker = useRef<HTMLInputElement>(null);
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 160)}px`;
  }, [text]);

  const submit = async () => {
    if ((!text.trim() && !attachments.length) || chat.busy) return;
    const t = text;
    setText("");
    if (!(await onSend(t))) setText(t);
  };
  // Files from the phone are saved on the computer first (agent_attach), then attached by path.
  const attach = async (files: File[]) => {
    for (const f of files) {
      setUploading((n) => n + 1);
      try {
        const path = await host<string>("agent_attach", { sessionId: chat.sessionId, name: f.name || "pasted", data: await toBase64(f) });
        setAttachments((a) => (a.includes(path) ? a : [...a, path]));
      } catch (e) {
        onError(`${f.name}: ${errMsg(e)}`);
      } finally {
        setUploading((n) => n - 1);
      }
    }
  };
  const p = POLICIES.find((x) => x.id === policy)!;
  const PolicyIcon = policy === "bypass" ? ShieldOff : policy === "auto-edit" ? ShieldCheck : Shield;
  const { pct } = usedPct(chat, models);
  const r = 6, circ = 2 * Math.PI * r;
  return (
    <div className="px-3 pt-2" style={{ paddingBottom: "max(10px, env(safe-area-inset-bottom))" }}>
      <div className="rounded-2xl border border-line bg-panel transition-colors focus-within:border-accent/60">
        {attachments.length > 0 && (
          <div className="flex flex-wrap gap-1.5 px-3 pt-3">
            {attachments.map((a) => <Chip key={a} path={a} onRemove={() => setAttachments((x) => x.filter((y) => y !== a))} />)}
          </div>
        )}
        <textarea ref={box} rows={1} value={text} onChange={(e) => setText(e.target.value)}
          onPaste={(e) => { const fs = [...e.clipboardData.files]; if (fs.length) { e.preventDefault(); void attach(fs); } }}
          placeholder={chat.busy ? "Working… type your next message" : "Ask DevCat Agent to build, fix or explain"}
          className="block max-h-40 min-h-11 w-full resize-none border-0 bg-transparent px-3.5 pt-3 pb-1 text-[15px] leading-snug outline-none placeholder:text-faint" />
        <div className="flex items-center gap-1.5 px-2 pt-1 pb-2">
          <input ref={picker} type="file" multiple hidden onChange={(e) => { const fs = Array.from(e.target.files ?? []); e.target.value = ""; void attach(fs); }} />
          <button aria-label="Attach files" disabled={uploading > 0} onClick={() => picker.current?.click()} className="flex size-9 shrink-0 items-center justify-center rounded-full text-muted active:bg-hover disabled:opacity-60">
            {uploading > 0 ? <Spinner className="size-4" /> : <Paperclip className="size-4" />}
          </button>
          <label className={clsx("relative flex h-8 shrink-0 items-center gap-1 rounded-full border px-2.5 text-[12px]", policy === "bypass" ? "border-warn/40 bg-warn/10 text-warn" : "border-line text-muted")}>
            <PolicyIcon className="size-3.5" />{p.label}<ChevronDown className="size-3 opacity-60" />
            <select value={policy} onChange={(e) => onPolicy(e.target.value as AgentPolicy)} className="absolute inset-0 opacity-0" aria-label="Permission mode">
              {POLICIES.map((x) => <option key={x.id} value={x.id}>{x.label}: {x.hint}</option>)}
            </select>
          </label>
          <label className="relative flex h-8 min-w-0 items-center gap-1 rounded-full bg-hover/70 px-2.5 text-[12px] text-muted">
            <Cpu className="size-3.5 shrink-0" /><span className="truncate">{chat.model}</span>
            {models.length > 0 && <ChevronDown className="size-3 shrink-0 opacity-60" />}
            {models.length > 0 && (
              <select value={chat.model} onChange={(e) => onModel(e.target.value)} className="absolute inset-0 opacity-0" aria-label="Model">
                {!models.some((m) => m.id === chat.model) && <option value={chat.model}>{chat.model}</option>}
                {models.map((m) => <option key={m.id} value={m.id}>{m.id}{m.context ? ` · ${fmtTokens(m.context)}` : ""}</option>)}
              </select>
            )}
          </label>
          <span className={clsx("ml-auto flex shrink-0 items-center gap-1 text-[11px]", pct >= 85 ? "text-danger" : pct >= 60 ? "text-warn" : "text-faint")} title="Context used">
            <svg viewBox="0 0 16 16" className="size-3.5 -rotate-90">
              <circle cx="8" cy="8" r={r} fill="none" stroke="currentColor" strokeOpacity="0.25" strokeWidth="2.2" />
              <circle cx="8" cy="8" r={r} fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeDasharray={`${(circ * pct) / 100} ${circ}`} />
            </svg>
            {pct}%
          </span>
          {chat.busy ? (
            <button aria-label="Stop" onClick={onStop} className="flex size-9 shrink-0 items-center justify-center rounded-full bg-fg text-app active:opacity-85"><Square className="size-3.5 fill-current" /></button>
          ) : (
            <button aria-label="Send" onClick={() => void submit()} disabled={!text.trim() && !attachments.length}
              className="flex size-9 shrink-0 items-center justify-center rounded-full bg-accent text-accent-fg active:brightness-90 disabled:bg-hover disabled:text-faint"><ArrowUp className="size-5" /></button>
          )}
        </div>
      </div>
    </div>
  );
}
