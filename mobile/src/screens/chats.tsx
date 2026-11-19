// Chats (the first tab): what needs you on top, a box to ask an agent straight away (in a
// project folder, with DevCat Agent, Claude Code or another agent), then the agents running and
// the recent ones. Terminals without an agent are in the Terminal tab (terminals.tsx).
import clsx from "clsx";
import { AlertTriangle, ArrowUp, Bot, Check, CheckCircle2, ChevronDown, ChevronRight, Clock, FolderOpen, Ghost, History, ImagePlus, Sparkles, TerminalSquare, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { claim, focusFolders, focusedProject, useFocus, useInFocus } from "../lib/focus";
import { host, nav, refreshAgents, toastErr, useApp } from "../lib/store";
import type { AgentChatInfo, AgentStatus, AiProvider, AiUsage, ClaudeProject, PtyInfo } from "../lib/types";
import { Btn, Empty, ErrorBox, Input, Screen, Section, Sheet, Spinner, TextArea, ago, duration, folderName, useLoad } from "../ui";
import { uploadPasteImage } from "../lib/transfer";
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
  const project = focusedProject(focus);
  const pick = (cwd: string) => { onPick(cwd); onClose(); };
  // under a project's focus: its folders first, then the other recent ones (picking one adds it)
  const mine = [...own.map((cwd) => ({ cwd })), ...projects.sorted.filter((p) => shown(p.cwd) && !own.includes(p.cwd))];
  const others = project ? projects.sorted.filter((p) => !shown(p.cwd)) : [];
  const list = project ? mine : projects.sorted;
  const row = (p: { cwd: string }) => (
    <button key={p.cwd} onClick={() => pick(p.cwd)} className={clsx("flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left active:bg-hover", value === p.cwd ? "bg-accent/10 ring-1 ring-accent/60" : "bg-raised")}>
      <FolderOpen className="size-5 shrink-0 text-muted" />
      <span className="min-w-0 flex-1"><span className="block truncate font-semibold">{folderName(p.cwd)}</span><span className="block truncate text-[11px] text-muted">{p.cwd}</span></span>
      {value === p.cwd && <Check className="size-4 text-accent" />}
    </button>
  );
  return (
    <Sheet open={open} onClose={onClose} title={title}>
      {projects.error && <ErrorBox error={projects.error} />}
      {projects.loading && <Spinner />}
      {project && <Section>{`In ${project.name}`}</Section>}
      {project && mine.length === 0 && <div className="text-[12.5px] text-muted">No folders yet. Pick one below: it becomes part of {project.name} on this phone.</div>}
      {project && <div className="space-y-1.5">{mine.slice(0, 8).map(row)}</div>}
      {project && others.length > 0 && <Section>Other folders</Section>}
      {project && <div className="space-y-1.5">{others.slice(0, 8).map(row)}</div>}
      <div className={clsx("space-y-1.5", project && "hidden")}>
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
type AgentChoice = { id: string; name: string; chat: boolean; usage?: AiUsage };

function useAgentChoices(): AgentChoice[] {
  const providers = useLoad(() => host<AiProvider[]>("ai_providers_list"), []);
  // plan usage of Claude Code / Antigravity logins (a computer without `ai_usage` leaves it out)
  const usage = useLoad(() => host<AiUsage[]>("ai_usage", {}).catch(() => [] as AiUsage[]), []);
  return useMemo(() => {
    const list = providers.data ?? [];
    const of = (id: string) => (usage.data ?? []).find((u) => u.providerId === id);
    const agent = list.filter((p) => p.kind === "agent").map((p) => ({ id: p.id, name: p.name, chat: true }));
    const others = list.filter((p) => p.kind !== "agent" && p.id !== "claude").map((p) => ({ id: p.id, name: p.name, chat: false, usage: of(p.id) }));
    return [...agent, { id: "", name: "Claude Code", chat: false, usage: of("claude") }, ...others];
  }, [providers.data, usage.data]);
}

/** "58% used · Weekly", the most used limit of a connection's plan. */
function usageLine(u?: AiUsage): string | null {
  if (!u || u.percent == null || !u.windows.length) return null;
  const top = u.windows.reduce((a, b) => (b.percent > a.percent ? b : a));
  return `${Math.round(top.percent)}% used · ${top.label}`;
}
const usageClass = (u?: AiUsage) => (u?.percent == null ? "text-muted" : u.percent >= 90 ? "text-danger" : u.percent >= 75 ? "text-warn" : "text-muted");

/** Start an agent: a DevCat Agent chat, or a terminal running Claude Code / another CLI agent.
 *  `images` go with the first message: attached in a chat; for a terminal agent saved on the
 *  computer (paste_dir) and their paths put after the message. */
export async function startAgent(cwd: string, choice: AgentChoice, message: string, images: File[] = []) {
  claim(cwd);
  if (choice.chat) {
    nav.push({ s: "agentchat", cwd, providerId: choice.id, sessionId: null, first: message || undefined, firstFiles: images.length ? images : undefined });
    return;
  }
  const paths: string[] = [];
  for (const f of images) paths.push(await uploadPasteImage(f));
  const prompt = [message, ...paths.map((p) => (p.includes(" ") ? `"${p}"` : p))].filter(Boolean).join(" ");
  const { cols, rows } = termSize();
  const info = await host<PtyInfo>("pty_open", { cwd, program: choice.id ? null : "claude", args: prompt ? [prompt] : [], cols, rows, providerId: choice.id || null });
  useApp.setState({ terms: [...useApp.getState().terms.filter((t) => t.id !== info.id), info] });
  nav.push({ s: "terminal", id: info.id });
}

/** A picked image, small, with a button to take it out again. */
function Thumb({ file, onRemove }: { file: File; onRemove: () => void }) {
  const [url, setUrl] = useState("");
  useEffect(() => {
    const u = URL.createObjectURL(file);
    setUrl(u);
    return () => URL.revokeObjectURL(u);
  }, [file]);
  return (
    <span className="relative size-14 shrink-0 overflow-hidden rounded-lg border border-line bg-hover">
      {url && <img src={url} alt={file.name} className="size-full object-cover" />}
      <button aria-label="Remove image" onClick={onRemove} className="absolute top-0.5 right-0.5 flex size-5 items-center justify-center rounded-full bg-black/60 text-white"><X className="size-3" /></button>
    </span>
  );
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
  /** Images to send with the first message. */
  const [images, setImages] = useState<File[]>([]);
  const picker = useRef<HTMLInputElement>(null);
  const addImages = (fs: File[]) => setImages((x) => [...x, ...fs.filter((f) => f.type.startsWith("image/"))]);
  /** A ghost chat: DevCat Agent in an empty scratch folder, keeping nothing (agent.rs). */
  const [ghost, setGhost] = useState(false);
  // a folder of the focused project, else the most recent one
  useEffect(() => {
    if (cwd && shown(cwd)) return;
    const next = focusFolders(focus)[0] ?? projects.sorted.find((p) => shown(p.cwd))?.cwd ?? projects.sorted[0]?.cwd ?? "";
    if (next !== cwd) setCwd(next);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projects.sorted, focus]);
  const chatAgent = choices.find((c) => c.chat);
  const agent = ghost ? chatAgent : choices.find((c) => c.id === agentId) ?? choices[0];
  const pickFolder = (c: string) => { setCwd(c); localStorage.setItem(KEY_FOLDER, c); };
  const pickAgent = (c: AgentChoice) => { setAgentId(c.id); localStorage.setItem(KEY_AGENT, c.id); setSheet(null); };
  const go = async () => {
    if (ghost && agent) {
      nav.push({ s: "agentchat", cwd: "", providerId: agent.id, sessionId: null, first: text.trim() || undefined, firstFiles: images.length ? images : undefined, ghost: true });
      setText("");
      setImages([]);
      return;
    }
    if (!cwd || !agent) return;
    setBusy(true);
    try {
      await startAgent(cwd, agent, text.trim(), images);
      setText("");
      setImages([]);
    } catch (e) {
      toastErr(e);
    } finally {
      setBusy(false);
    }
  };
  if (readonly) return null;
  return (
    <div className={clsx("rounded-2xl border p-2.5 shadow-sm", ghost ? "border-fg/40 border-dashed bg-panel" : "border-line bg-raised focus-within:border-accent/70")}>
      {images.length > 0 && (
        <div className="flex flex-wrap gap-1.5 px-1 pb-2">
          {images.map((f, i) => <Thumb key={i} file={f} onRemove={() => setImages((x) => x.filter((y) => y !== f))} />)}
        </div>
      )}
      <TextArea rows={2} value={text} disabled={offline}
        onPaste={(e) => { const fs = Array.from(e.clipboardData.files).filter((f) => f.type.startsWith("image/")); if (fs.length) { e.preventDefault(); addImages(fs); } }} placeholder={offline ? "Connect to the computer to start an agent" : ghost ? "Ask anything. Nothing of this chat is kept…" : "Ask an agent to build, fix or explain…"}
        className="max-h-40 min-h-14 resize-none border-0 bg-transparent px-1 focus:border-0"
        onChange={(e) => setText(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey && (text.trim() || images.length)) { e.preventDefault(); void go(); } }} />
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
              <span className="truncate font-medium">{agent?.name ?? "Agent"}</span>{agent?.usage?.percent != null && <span className={clsx("shrink-0 tabular-nums", usageClass(agent.usage))}>{Math.round(agent.usage.percent)}%</span>}<ChevronDown className="size-3.5 shrink-0 text-faint" />
            </button>
          </>
        )}
        <span className="flex-1" />
        <input ref={picker} type="file" accept="image/*" multiple hidden onChange={(e) => { const fs = Array.from(e.target.files ?? []); e.target.value = ""; addImages(fs); }} />
        <button aria-label="Add image" disabled={offline || busy} onClick={() => picker.current?.click()} className="flex size-9 shrink-0 items-center justify-center rounded-full text-muted active:bg-hover disabled:opacity-40">
          <ImagePlus className="size-[18px]" />
        </button>
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
            <span className="min-w-0 flex-1"><span className="block font-semibold">{c.name}</span><span className="block text-[11.5px] text-muted">{c.chat ? "A chat with approvals as buttons" : "Runs in a terminal on the computer"}</span>{usageLine(c.usage) && <span className={clsx("block text-[11.5px] tabular-nums", usageClass(c.usage))}>{usageLine(c.usage)}</span>}</span>
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

/** Rows by time, newest on top. */
const newestFirst = (rows: { at: number; row: React.ReactNode }[]) => [...rows].sort((a, b) => b.at - a.at).map((r) => r.row);

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
  const focusName = focusedProject(useFocus())?.name;
  const [, tick] = useState(0);
  useEffect(() => {
    void refreshAgents();
    // status changes arrive as events: the list is only checked now and then, the times tick here
    const t = setInterval(() => { if (!document.hidden) void refreshAgents(); }, 15_000);
    const clock = setInterval(() => { if (!document.hidden) tick((x) => x + 1); }, 5000);
    return () => { clearInterval(t); clearInterval(clock); };
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
          {newestFirst([
            ...g.workingChats.map((c) => ({ at: c.since, row: (
              <Row key={c.sessionId} onClick={() => openChat(c)} icon={<Spinner className="size-4 text-warn" />} title={c.title || folderName(c.cwd)} meta={`${folderName(c.cwd)} · ${c.providerName}`} right={duration(Date.now() - c.since)} />
            ) })),
            ...g.workingTerms.map((t) => ({ at: agents[t.id]?.since ?? t.startedAt, row: (
              <Row key={t.id} onClick={() => openTerm(t)} icon={<Spinner className="size-4 text-warn" />} title={folderName(cwdOf(t, agents[t.id]))} meta={`${t.program} · ${by(t.origin)}`} right={duration(Date.now() - (agents[t.id]?.since ?? t.startedAt))} />
            ) })),
          ])}
        </div>
      )}

      {recent > 0 && <Section>Recent</Section>}
      {recent > 0 && (
        <div>
          {newestFirst([
            ...g.doneChats.map((c) => ({ at: c.since, row: (
              <Row key={c.sessionId} onClick={() => openChat(c)} icon={<Bot className="size-4 text-ok" />} title={c.title || folderName(c.cwd)} meta={`${folderName(c.cwd)} · ${c.providerName}`} right={ago(c.since)} />
            ) })),
            ...g.doneTerms.map((t) => {
              const a = agents[t.id];
              return { at: a?.since ?? t.startedAt, row: (
                <Row key={t.id} onClick={() => openTerm(t)} icon={a ? <CheckCircle2 className="size-4 text-ok" /> : <Clock className="size-4 text-muted" />} title={folderName(cwdOf(t, a))}
                  meta={a?.lastTurnMs ? `${t.program} · took ${duration(a.lastTurnMs)}` : `${t.program} · ${by(t.origin)}`} right={ago(a?.since ?? t.startedAt)} />
              ) };
            }),
          ])}
        </div>
      )}

      <PastSessions />

      {online && running + recent === 0 && (focusName || needs === 0) && (
        focusName ? (
          <Empty icon={<Sparkles className="size-8" />} title={`Nothing running in ${focusName}`}>Chats and terminals in its folders show here. Ask something above: the folder you pick becomes part of {focusName}.</Empty>
        ) : (
          <Empty icon={<Sparkles className="size-8" />} title="No agents running">Ask something above. The agent runs on your computer and keeps going when you close this app.</Empty>
        )
      )}
      <button onClick={() => nav.push({ s: "sessions" })} className="flex w-full items-center justify-center gap-1.5 py-2 text-[13px] font-semibold text-accent">
        <History className="size-4" />All sessions
      </button>
    </Screen>
  );
}

/** Earlier work, by project folder (Claude Code's sessions): opens that folder's history. */
function PastSessions() {
  const projects = useProjects();
  const shown = useInFocus();
  const list = projects.sorted.filter((p) => shown(p.cwd)).slice(0, 5);
  if (list.length === 0) return null;
  return (
    <>
      <Section>Past sessions</Section>
      <div>
        {list.map((p) => (
          <Row key={p.cwd} onClick={() => nav.push({ s: "sessions", cwd: p.cwd })} icon={<History className="size-4 text-muted" />} title={folderName(p.cwd)}
            meta={`${p.sessionCount} session${p.sessionCount === 1 ? "" : "s"} · ${p.cwd}`} right={p.lastActive ? ago(p.lastActive) : undefined} />
        ))}
      </div>
    </>
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
