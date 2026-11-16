// Chats (the first tab): what needs you on top, a box to ask an agent straight away (in a
// project folder, with DevCat Agent, Claude Code or another agent), then the agents running and
// the recent ones. Terminals without an agent are in the Terminal tab (terminals.tsx).
import clsx from "clsx";
import { AlertTriangle, ArrowUp, Bot, Check, CheckCircle2, ChevronDown, ChevronRight, Clock, FolderOpen, Ghost, History, Sparkles, TerminalSquare } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { focusFolders, useFocus, useInFocus } from "../lib/focus";
import { host, nav, refreshAgents, toastErr, useApp } from "../lib/store";
import type { AgentChatInfo, AgentStatus, AiProvider, ClaudeProject, PtyInfo } from "../lib/types";
import { Btn, Empty, ErrorBox, Input, Screen, Section, Sheet, Spinner, TextArea, ago, duration, folderName, useLoad } from "../ui";
import { termSize } from "./agents";
import { ProjectBar } from "./focus";

const KEY_FOLDER = "devcat.mobile.lastFolder";
const KEY_AGENT = "devcat.mobile.lastAgent";

/** The project folder: where Claude says it works (hooks), else where the terminal started. */
export const cwdOf = (t: PtyInfo | undefined, a: AgentStatus | undefined) => a?.cwd || t?.cwd || "";
const by = (o: string) => (o.startsWith("phone:") ? `from ${o.slice(6)}` : "on the computer");

/** Projects on the computer, most recent first (Claude Code's list). */
export function useProjects() {
  const p = useLoad(() => host<ClaudeProject[]>("claude_projects"), []);
  const sorted = useMemo(() => [...(p.data ?? [])].sort((a, b) => (b.lastActive ?? "").localeCompare(a.lastActive ?? "")), [p.data]);
  return { ...p, sorted };
}

/** Pick a project folder on the computer (recent ones, or a typed path). */
export function FolderSheet({ open, value, onClose, onPick, title = "Project folder" }: { open: boolean; value: string; onClose: () => void; onPick: (cwd: string) => void; title?: string }) {
  const projects = useProjects();
  const home = useLoad(() => host<string | null>("home_dir"), []);
  const [other, setOther] = useState("");
  const focus = useFocus();
  const shown = useInFocus();
  const own = focusFolders(focus);
  const pick = (cwd: string) => { onPick(cwd); onClose(); };
  // under a project's focus: its folders first, then recent ones inside them
  const list = [...own.map((cwd) => ({ cwd })), ...projects.sorted.filter((p) => shown(p.cwd) && !own.includes(p.cwd))];
  return (
    <Sheet open={open} onClose={onClose} title={title}>
      {projects.error && <ErrorBox error={projects.error} />}
      {projects.loading && <Spinner />}
      <div className="space-y-1.5">
        {list.slice(0, 8).map((p) => (
          <button key={p.cwd} onClick={() => pick(p.cwd)} className={clsx("flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left active:bg-hover", value === p.cwd ? "bg-accent/10 ring-1 ring-accent/60" : "bg-raised")}>
            <FolderOpen className="size-5 shrink-0 text-muted" />
            <span className="min-w-0 flex-1"><span className="block truncate font-semibold">{folderName(p.cwd)}</span><span className="block truncate text-[11px] text-muted">{p.cwd}</span></span>
            {value === p.cwd && <Check className="size-4 text-accent" />}
          </button>
        ))}
        {home.data && (
          <button onClick={() => pick(home.data!)} className="flex w-full items-center gap-3 rounded-xl bg-raised px-3 py-2.5 text-left active:bg-hover">
            <FolderOpen className="size-5 shrink-0 text-muted" /><span className="min-w-0 flex-1"><span className="block font-semibold">Home folder</span><span className="block truncate text-[11px] text-muted">{home.data}</span></span>
          </button>
        )}
      </div>
      <div className="flex gap-2 pt-1">
        <Input value={other} placeholder="Another folder, e.g. ~/Projects/api" onChange={(e) => setOther(e.target.value)} />
        <Btn disabled={!other.trim()} onClick={() => pick(other.trim())}>Use</Btn>
      </div>
    </Sheet>
  );
}

/** "" = Claude Code in a terminal; otherwise an AI provider of the computer. */
type AgentChoice = { id: string; name: string; chat: boolean };

function useAgentChoices(): AgentChoice[] {
  const providers = useLoad(() => host<AiProvider[]>("ai_providers_list"), []);
  return useMemo(() => {
    const list = providers.data ?? [];
    const agent = list.filter((p) => p.kind === "agent").map((p) => ({ id: p.id, name: p.name, chat: true }));
    const others = list.filter((p) => p.kind !== "agent" && p.id !== "claude").map((p) => ({ id: p.id, name: p.name, chat: false }));
    return [...agent, { id: "", name: "Claude Code", chat: false }, ...others];
  }, [providers.data]);
}

/** Start an agent: a DevCat Agent chat, or a terminal running Claude Code / another CLI agent. */
export async function startAgent(cwd: string, choice: AgentChoice, message: string) {
  if (choice.chat) {
    nav.push({ s: "agentchat", cwd, providerId: choice.id, sessionId: null, first: message || undefined });
    return;
  }
  const { cols, rows } = termSize();
  const info = await host<PtyInfo>("pty_open", { cwd, program: choice.id ? null : "claude", args: message ? [message] : [], cols, rows, providerId: choice.id || null });
  useApp.setState({ terms: [...useApp.getState().terms.filter((t) => t.id !== info.id), info] });
  nav.push({ s: "terminal", id: info.id });
}

function Composer({ readonly, offline }: { readonly: boolean; offline: boolean }) {
  const projects = useProjects();
  const choices = useAgentChoices();
  const focus = useFocus();
  const shown = useInFocus();
  const [cwd, setCwd] = useState(() => localStorage.getItem(KEY_FOLDER) ?? "");
  const [agentId, setAgentId] = useState<string | null>(() => localStorage.getItem(KEY_AGENT));
  const [text, setText] = useState("");
  const [sheet, setSheet] = useState<"folder" | "agent" | null>(null);
  const [busy, setBusy] = useState(false);
  /** A ghost chat: DevCat Agent in an empty scratch folder, keeping nothing (agent.rs). */
  const [ghost, setGhost] = useState(false);
  // a folder of the focused project, else the most recent one
  useEffect(() => {
    if (cwd && shown(cwd)) return;
    const next = focusFolders(focus)[0] ?? projects.sorted.find((p) => shown(p.cwd))?.cwd ?? (focus.focus ? "" : projects.sorted[0]?.cwd ?? "");
    if (next !== cwd) setCwd(next);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projects.sorted, focus]);
  const chatAgent = choices.find((c) => c.chat);
  const agent = ghost ? chatAgent : choices.find((c) => c.id === agentId) ?? choices[0];
  const pickFolder = (c: string) => { setCwd(c); localStorage.setItem(KEY_FOLDER, c); };
  const pickAgent = (c: AgentChoice) => { setAgentId(c.id); localStorage.setItem(KEY_AGENT, c.id); setSheet(null); };
  const go = async () => {
    if (ghost && agent) {
      nav.push({ s: "agentchat", cwd: "", providerId: agent.id, sessionId: null, first: text.trim() || undefined, ghost: true });
      setText("");
      return;
    }
    if (!cwd || !agent) return;
    setBusy(true);
    try {
      await startAgent(cwd, agent, text.trim());
      setText("");
    } catch (e) {
      toastErr(e);
    } finally {
      setBusy(false);
    }
  };
  if (readonly) return null;
  return (
    <div className={clsx("rounded-2xl border p-2.5 shadow-sm", ghost ? "border-fg/40 border-dashed bg-panel" : "border-line bg-raised focus-within:border-accent/70")}>
      <TextArea rows={2} value={text} disabled={offline} placeholder={offline ? "Connect to the computer to start an agent" : ghost ? "Ask anything. Nothing of this chat is kept…" : "Ask an agent to build, fix or explain…"}
        className="max-h-40 min-h-14 resize-none border-0 bg-transparent px-1 focus:border-0"
        onChange={(e) => setText(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey && text.trim()) { e.preventDefault(); void go(); } }} />
      <div className="mt-1 flex items-center gap-1.5">
        {ghost ? (
          <span className="flex h-8 min-w-0 items-center gap-1 rounded-full bg-hover/60 px-2.5 text-[12.5px] text-muted">No project · keeps nothing</span>
        ) : (
          <>
            <button onClick={() => setSheet("folder")} className="flex h-8 min-w-0 items-center gap-1 rounded-full bg-hover px-2.5 text-[12.5px] active:bg-line">
              <FolderOpen className="size-3.5 shrink-0 text-muted" /><span className="truncate font-medium">{cwd ? folderName(cwd) : "Folder"}</span><ChevronDown className="size-3.5 shrink-0 text-faint" />
            </button>
            <button onClick={() => setSheet("agent")} className="flex h-8 min-w-0 items-center gap-1 rounded-full bg-hover px-2.5 text-[12.5px] active:bg-line">
              {agent?.chat ? <Sparkles className="size-3.5 shrink-0 text-accent" /> : <TerminalSquare className="size-3.5 shrink-0 text-muted" />}
              <span className="truncate font-medium">{agent?.name ?? "Agent"}</span><ChevronDown className="size-3.5 shrink-0 text-faint" />
            </button>
          </>
        )}
        <span className="flex-1" />
        {chatAgent && (
          <button aria-label="Ghost chat" aria-pressed={ghost} title="Ghost chat: keeps nothing" onClick={() => setGhost(!ghost)}
            className={clsx("flex size-9 shrink-0 items-center justify-center rounded-full", ghost ? "bg-fg text-app" : "text-muted active:bg-hover")}>
            <Ghost className="size-[18px]" />
          </button>
        )}
        <button aria-label="Start" disabled={offline || busy || (!ghost && !cwd)} onClick={() => void go()}
          className="flex size-10 shrink-0 items-center justify-center rounded-full bg-accent text-accent-fg active:brightness-90 disabled:opacity-40">
          {busy ? <Spinner className="text-accent-fg" /> : <ArrowUp className="size-5" />}
        </button>
      </div>
      <FolderSheet open={sheet === "folder"} value={cwd} onClose={() => setSheet(null)} onPick={pickFolder} />
      <Sheet open={sheet === "agent"} onClose={() => setSheet(null)} title="Agent">
        {choices.map((c) => (
          <button key={c.id || "claude"} onClick={() => pickAgent(c)} className={clsx("flex w-full items-center gap-3 rounded-xl px-3 py-3 text-left active:bg-hover", agent?.id === c.id ? "bg-accent/10 ring-1 ring-accent/60" : "bg-raised")}>
            {c.chat ? <Sparkles className="size-5 shrink-0 text-accent" /> : <TerminalSquare className="size-5 shrink-0 text-muted" />}
            <span className="min-w-0 flex-1"><span className="block font-semibold">{c.name}</span><span className="block text-[11.5px] text-muted">{c.chat ? "A chat with approvals as buttons" : "Runs in a terminal on the computer"}</span></span>
            {agent?.id === c.id && <Check className="size-4 text-accent" />}
          </button>
        ))}
      </Sheet>
    </div>
  );
}

/** One row of the lists: icon, title, a line under it, and time on the right. */
function Row({ icon, title, meta, right, onClick }: { icon: React.ReactNode; title: React.ReactNode; meta: React.ReactNode; right?: React.ReactNode; onClick: () => void }) {
  return (
    <button onClick={onClick} className="flex w-full items-center gap-3 rounded-xl px-2 py-2.5 text-left active:bg-hover">
      <span className="flex size-9 shrink-0 items-center justify-center rounded-full bg-raised">{icon}</span>
      <span className="min-w-0 flex-1"><span className="block truncate text-[14.5px] font-semibold">{title}</span><span className="block truncate text-[12px] text-muted">{meta}</span></span>
      {right && <span className="shrink-0 text-[11.5px] text-faint">{right}</span>}
      <ChevronRight className="size-4 shrink-0 text-faint" />
    </button>
  );
}

const openChat = (c: AgentChatInfo) => nav.push({ s: "agentchat", cwd: c.cwd, providerId: c.providerId, sessionId: c.sessionId });
const openTerm = (t: PtyInfo) => nav.push({ s: "terminal", id: t.id });

export function Chats() {
  const terms = useApp((s) => s.terms);
  const agents = useApp((s) => s.agents);
  const chats = useApp((s) => s.chats);
  const conn = useApp((s) => s.conn[s.current ?? ""]);
  const machine = useApp((s) => s.machines.find((m) => m.id === s.current));
  const readonly = machine?.access === "readonly";
  const shown = useInFocus();
  const [, tick] = useState(0);
  useEffect(() => {
    void refreshAgents();
    const t = setInterval(() => { if (!document.hidden) { void refreshAgents(); tick((x) => x + 1); } }, 5000);
    return () => clearInterval(t);
  }, []);
  const online = conn?.state === "online";

  const g = useMemo(() => {
    const agentTerms = terms.filter((t) => t.agent);
    const st = (t: PtyInfo) => agents[t.id]?.status;
    // what needs you shows whatever the focus; the rest follows it
    const mine = agentTerms.filter((t) => shown(cwdOf(t, agents[t.id])));
    const myChats = chats.filter((c) => shown(c.cwd));
    return {
      waitingTerms: agentTerms.filter((t) => st(t) === "waiting"),
      waitingChats: chats.filter((c) => c.waiting),
      workingTerms: mine.filter((t) => st(t) === "working"),
      workingChats: myChats.filter((c) => c.busy && !c.waiting),
      doneTerms: mine.filter((t) => st(t) !== "waiting" && st(t) !== "working"),
      doneChats: myChats.filter((c) => !c.busy && !c.waiting),
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [terms, agents, chats, shown]);
  const needs = g.waitingTerms.length + g.waitingChats.length;
  const running = g.workingTerms.length + g.workingChats.length;
  const recent = g.doneTerms.length + g.doneChats.length;

  return (
    <Screen
      title="Chats"
      sub={<>{machine?.name ?? "Computer"} · {online ? <span className="text-ok">● online</span> : conn?.state === "connecting" ? "connecting…" : <span className="text-faint">○ offline</span>}</>}
      right={<button aria-label="All sessions" onClick={() => nav.push({ s: "sessions" })} className="flex size-9 items-center justify-center rounded-full text-muted active:bg-hover"><History className="size-5" /></button>}
      tab="agents" className="space-y-3 p-3"
    >
      <ProjectBar />
      {!online && conn?.error && <ErrorBox error={conn.error} onRetry={() => void refreshAgents()} />}

      {needs > 0 && (
        <div className="space-y-2">
          {g.waitingChats.map((c) => (
            <NeedsYou key={c.sessionId} title={folderName(c.cwd)} who={c.providerName} text={c.waiting ?? ""} onOpen={() => openChat(c)} />
          ))}
          {g.waitingTerms.map((t) => (
            <NeedsYou key={t.id} title={folderName(cwdOf(t, agents[t.id]))} who={t.program} text={agents[t.id]?.message ?? "Waiting for you"} onOpen={() => openTerm(t)} />
          ))}
        </div>
      )}

      <Composer readonly={readonly} offline={!online} />

      {running > 0 && <Section>Running</Section>}
      {running > 0 && (
        <div>
          {g.workingChats.map((c) => (
            <Row key={c.sessionId} onClick={() => openChat(c)} icon={<Spinner className="size-4 text-warn" />} title={c.title || folderName(c.cwd)} meta={`${folderName(c.cwd)} · ${c.providerName}`} right={duration(Date.now() - c.since)} />
          ))}
          {g.workingTerms.map((t) => (
            <Row key={t.id} onClick={() => openTerm(t)} icon={<Spinner className="size-4 text-warn" />} title={folderName(cwdOf(t, agents[t.id]))} meta={`${t.program} · ${by(t.origin)}`} right={duration(Date.now() - (agents[t.id]?.since ?? t.startedAt))} />
          ))}
        </div>
      )}

      {recent > 0 && <Section>Recent</Section>}
      {recent > 0 && (
        <div>
          {g.doneChats.map((c) => (
            <Row key={c.sessionId} onClick={() => openChat(c)} icon={<Bot className="size-4 text-ok" />} title={c.title || folderName(c.cwd)} meta={`${folderName(c.cwd)} · ${c.providerName}`} right={ago(c.since)} />
          ))}
          {g.doneTerms.map((t) => {
            const a = agents[t.id];
            return (
              <Row key={t.id} onClick={() => openTerm(t)} icon={a ? <CheckCircle2 className="size-4 text-ok" /> : <Clock className="size-4 text-muted" />} title={folderName(cwdOf(t, a))}
                meta={a?.lastTurnMs ? `${t.program} · took ${duration(a.lastTurnMs)}` : `${t.program} · ${by(t.origin)}`} right={ago(a?.since ?? t.startedAt)} />
            );
          })}
        </div>
      )}

      {online && needs + running + recent === 0 && (
        <Empty icon={<Sparkles className="size-8" />} title="No agents yet">Ask something above. The agent runs on your computer and keeps going when you close this app.</Empty>
      )}
      <button onClick={() => nav.push({ s: "sessions" })} className="flex w-full items-center justify-center gap-1.5 py-2 text-[13px] font-semibold text-accent">
        <History className="size-4" />All sessions
      </button>
    </Screen>
  );
}

function NeedsYou({ title, who, text, onOpen }: { title: string; who: string; text: string; onOpen: () => void }) {
  return (
    <button onClick={onOpen} className="block w-full rounded-2xl border border-warn/50 bg-warn/[0.08] p-3 text-left active:bg-warn/15">
      <div className="flex items-center gap-2">
        <AlertTriangle className="size-4 shrink-0 text-warn" />
        <b className="min-w-0 flex-1 truncate">{title} needs you</b>
        <span className="shrink-0 text-[11px] text-muted">{who}</span>
      </div>
      <div className="mt-1 line-clamp-2 text-[13px]">{text}</div>
      <div className="mt-2 flex justify-end"><span className="rounded-full bg-warn px-3 py-1 text-[12.5px] font-bold text-accent-fg">Review</span></div>
    </button>
  );
}
