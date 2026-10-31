// The room tab's side panels: the thread (mentions of people and agents, replies, reactions, and
// messages pinned as decisions), suggestions waiting for people, and the history of applied
// changes (each can be reverted while its text is untouched).
import clsx from "clsx";
import { AtSign, Bot, Check, ChevronDown, ChevronRight, CornerDownRight, Loader2, Paperclip, Pin as PinIcon, Reply, RotateCcw, Send, SmilePlus, Square, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { toast } from "sonner";
import { Badge, Button, Textarea, Spin } from "../../components/ui";
import { errMsg } from "../../lib/api";
import { isFileDrag } from "../files/drop";
import { label, REACTIONS, rooms, type Activity, type HistoryItem, type Message, type Pin, type Reaction, type RoomView, type Suggestion } from "./api";
import { ActivityFeed } from "./FileHistory";
import { shareFiles } from "./share";
import { useRooms } from "./store";

const time = (ms: number) => {
  const d = new Date(ms);
  const today = new Date().toDateString() === d.toDateString();
  return today ? d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }) : d.toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
};

const escapeRe = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
// a character that would make a name part of a longer one (as the room agent reads names)
const namePart = /[\p{L}\p{N}_\\/-]/u;

/** Message text with @handles picked out, and the room's files it names as links to them. */
function Rich({ text, handles, files, onOpen }: { text: string; handles: string[]; files: string[]; onOpen: (path: string) => void }) {
  const parts = useMemo(() => {
    const known = new Set([...handles.map((h) => h.toLowerCase()), "@agents", "@everyone"]);
    // longest first, so "plan v2.md" wins over "plan"
    const names = [...files].filter((f) => f.length > 2).sort((a, b) => b.length - a.length).map(escapeRe);
    const re = new RegExp(`@[\\w-]+${names.length ? `|${names.join("|")}` : ""}`, "g");
    const out: ReactNode[] = [];
    let at = 0;
    for (const m of text.matchAll(re)) {
      const [word, i] = [m[0], m.index ?? 0];
      const handle = word.startsWith("@") && known.has(word.toLowerCase());
      const before = text[i - 1] ?? "";
      const after = text[i + word.length] ?? "";
      const file = !word.startsWith("@") && !namePart.test(before) && before !== "." && !namePart.test(after);
      if (!handle && !file) continue;
      if (i > at) out.push(text.slice(at, i));
      out.push(handle
        ? <span key={i} className="rounded bg-accent/15 px-0.5 font-medium text-accent">{word}</span>
        : <button key={i} onClick={() => onOpen(word)} title="Open it in the room" className="inline rounded bg-hover px-0.5 font-medium text-accent underline decoration-dotted underline-offset-2 hover:decoration-solid"><Paperclip className="mr-0.5 inline size-3" />{word}</button>);
      at = i + word.length;
    }
    if (at < text.length) out.push(text.slice(at));
    return out;
  }, [text, handles, files, onOpen]);
  return <div className="text-[13px] break-words whitespace-pre-wrap">{parts}</div>;
}

/** A message's reactions, one chip per emoji (yours highlighted; a click adds or takes back yours),
 *  and the picker. */
function Reactions({ room, message, reactions, me, canReact }: { room: string; message: string; reactions: Reaction[]; me: string; canReact: boolean }) {
  const [picking, setPicking] = useState(false);
  const groups = REACTIONS.map((e) => ({ e, by: reactions.filter((r) => r.emoji === e) })).filter((g) => g.by.length);
  const react = (e: string) => {
    setPicking(false);
    void rooms.react(room, message, e).catch((er) => toast.error(errMsg(er)));
  };
  if (!groups.length && !picking) return canReact ? <button className="mt-0.5 hidden text-faint group-hover:inline-block hover:text-fg" title="React" onClick={() => setPicking(true)}><SmilePlus className="size-3.5" /></button> : null;
  return (
    <div className="mt-1 flex flex-wrap items-center gap-1">
      {groups.map((g) => (
        <button key={g.e} disabled={!canReact} onClick={() => react(g.e)} title={g.by.map((r) => r.by.name).join(", ")}
          className={clsx("flex items-center gap-0.5 rounded-full border px-1.5 text-[11px]", g.by.some((r) => r.by.member === me) ? "border-accent/60 bg-accent/15 text-fg" : "border-line bg-app text-muted")}>
          {g.e}<span>{g.by.length}</span>
        </button>
      ))}
      {canReact && (picking
        ? <span className="flex items-center gap-0.5 rounded-full border border-line bg-app px-1">{REACTIONS.map((e) => <button key={e} className="px-0.5 hover:scale-125" onClick={() => react(e)}>{e}</button>)}<button className="text-faint hover:text-fg" title="Close" onClick={() => setPicking(false)}><X className="size-3" /></button></span>
        : <button className="text-faint hover:text-fg" title="React" onClick={() => setPicking(true)}><SmilePlus className="size-3.5" /></button>)}
    </div>
  );
}

/** `files`: the room's files, linked where a message names them (`onOpen` opens one). `canShare`:
 *  files dropped or pasted here are shared with the room and named in the message being written.
 *  `canEdit`: may pin messages as decisions. */
export function ThreadPanel({ room, view, messages, pins, reactions, files, onOpen, canShare, canEdit }: {
  room: string; view: RoomView; messages: Message[]; pins: Pin[]; reactions: Reaction[]; files: string[]; onOpen: (path: string) => void; canShare: boolean; canEdit: boolean;
}) {
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const [dragging, setDragging] = useState(false);
  const [sharing, setSharing] = useState(false);
  const [replyTo, setReplyTo] = useState<Message | null>(null);
  const [decisionsOpen, setDecisionsOpen] = useState(true);
  const running = useRooms((s) => s.agents[room]) ?? view.agentRunning;
  const agentError = useRooms((s) => s.agentErrors[room]);
  const end = useRef<HTMLDivElement>(null);
  const handles = [...Object.values(view.handles), ...Object.values(view.people ?? {})];
  const byId = useMemo(() => new Map(messages.map((m) => [m.id, m])), [messages]);
  const pinned = useMemo(() => new Set(pins.map((p) => p.message)), [pins]);
  useEffect(() => { void end.current?.scrollIntoView({ block: "end" }); }, [messages.length]);
  const jump = (id: string) => document.getElementById(`room-msg-${id}`)?.scrollIntoView({ block: "center", behavior: "smooth" });
  const pin = (m: Message, on: boolean) => void rooms.pin(room, m.id, on).catch((e) => toast.error(errMsg(e)));
  const send = async () => {
    const t = text.trim();
    if (!t) return;
    setSending(true);
    try {
      await rooms.post(room, t, replyTo?.id);
      setText("");
      setReplyTo(null);
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setSending(false);
    }
  };
  const mention = (h: string) => setText((t) => `${t}${t && !t.endsWith(" ") ? " " : ""}${h} `);
  // share, then name them in the message, so people (and a mentioned agent) know which files it means
  const attach = async (from: DataTransfer | File[]) => {
    setSharing(true);
    try {
      const names = await shareFiles(room, from);
      if (names.length) setText((t) => `${t}${t && !/\s$/.test(t) ? " " : ""}${names.join(", ")} `);
    } finally {
      setSharing(false);
    }
  };
  const myHandle = view.handles[view.me];
  return (
    <div className="relative flex h-full min-h-0 flex-col"
      onDragOver={(e) => { if (canShare && isFileDrag(e.dataTransfer)) { e.preventDefault(); e.stopPropagation(); setDragging(true); } }}
      onDragLeave={(e) => { if (!e.currentTarget.contains(e.relatedTarget as Node)) setDragging(false); }}
      onDrop={(e) => {
        if (!canShare || !isFileDrag(e.dataTransfer)) return;
        // here, not in the room around it: shared and named in the message
        e.preventDefault();
        e.stopPropagation();
        setDragging(false);
        void attach(e.dataTransfer);
      }}>
      {dragging && (
        <div className="pointer-events-none absolute inset-2 z-30 flex items-center justify-center rounded-lg border-2 border-dashed border-accent/60 bg-app/70 px-4">
          <div className="flex flex-col items-center gap-1.5 rounded-lg border border-line bg-raised px-4 py-3 text-center text-[13px] text-fg shadow-lg">
            <Paperclip className="size-5 text-accent" />Drop to share with the room and name it in your message
          </div>
        </div>
      )}
      {pins.length > 0 && (
        <div className="max-h-40 shrink-0 overflow-auto border-b border-line bg-warn/5 px-3 py-1.5">
          <button className="flex w-full items-center gap-1 text-[11px] font-semibold tracking-wide text-warn uppercase" onClick={() => setDecisionsOpen((v) => !v)}>
            {decisionsOpen ? <ChevronDown className="size-3" /> : <ChevronRight className="size-3" />}<PinIcon className="size-3" />Decisions ({pins.length})
          </button>
          {decisionsOpen && pins.map((p) => {
            const m = byId.get(p.message);
            return m ? (
              <button key={p.message} onClick={() => jump(m.id)} title={`Pinned by ${label(p.by)} · ${time(p.at)}`} className="block w-full truncate py-0.5 text-left text-[12px] text-fg hover:underline">
                {m.text}
              </button>
            ) : null;
          })}
        </div>
      )}
      <div className="min-h-0 flex-1 space-y-2 overflow-auto p-3">
        {messages.length === 0 && <div className="py-8 text-center text-[12px] text-muted">Nothing said yet. Mention an agent to bring it in, e.g. <span className="font-mono">{myHandle ?? "@you-agent"}</span>.</div>}
        {messages.map((m) => {
          const reply = m.replyTo ? byId.get(m.replyTo) : undefined;
          const mine = m.author.member === view.me && !m.author.agent;
          const isPinned = pinned.has(m.id);
          return (
            <div id={`room-msg-${m.id}`} key={m.id} className={clsx("group rounded-md px-2.5 py-1.5", isPinned && "ring-1 ring-warn/50", m.author.agent ? "border border-accent/25 bg-accent/5" : mine ? "bg-hover" : "bg-raised")}>
              <div className="mb-0.5 flex items-center gap-1.5 text-[11px] text-muted">
                {m.author.agent && <Bot className="size-3 text-accent" />}
                <span className={clsx("font-semibold", m.author.agent ? "text-accent" : "text-fg")}>{label(m.author)}</span>
                <span>{time(m.at)}</span>
                {isPinned && <span className="flex items-center gap-0.5 text-warn" title="Pinned as a decision"><PinIcon className="size-3" />decision</span>}
                <span className="ml-auto hidden items-center gap-1.5 group-hover:flex">
                  {!view.summary.removed && <button className="hover:text-fg" title="Reply" onClick={() => setReplyTo(m)}><Reply className="size-3.5" /></button>}
                  {canEdit && <button className="hover:text-fg" title={isPinned ? "Unpin" : "Pin as a decision"} onClick={() => pin(m, !isPinned)}><PinIcon className={clsx("size-3.5", isPinned && "text-warn")} /></button>}
                </span>
              </div>
              {reply && (
                <button onClick={() => jump(reply.id)} className="mb-1 flex w-full items-start gap-1 text-left text-[11px] text-faint hover:text-muted">
                  <CornerDownRight className="mt-0.5 size-3 shrink-0" />
                  <span className="truncate">{label(reply.author)}: {reply.text}</span>
                </button>
              )}
              <Rich text={m.text} handles={handles} files={files} onOpen={onOpen} />
              <Reactions room={room} message={m.id} reactions={reactions.filter((r) => r.message === m.id)} me={view.me} canReact={!view.summary.removed} />
            </div>
          );
        })}
        <div ref={end} />
      </div>
      {running && (
        <div className="flex items-center gap-2 border-t border-line px-3 py-1.5 text-[12px] text-accent">
          <Spin icon={Loader2} className="size-3.5" />Your agent is working…
          <Button size="sm" variant="ghost" className="ml-auto" onClick={() => void rooms.agentStop(room)}><Square className="size-3" />Stop</Button>
        </div>
      )}
      {!running && agentError && <div className="border-t border-line px-3 py-1.5 text-[11px] text-danger">Your agent: {agentError}</div>}
      <div className="space-y-1.5 border-t border-line p-2">
        {replyTo && (
          <div className="flex items-center gap-1.5 rounded bg-hover px-2 py-1 text-[11px] text-muted">
            <Reply className="size-3 shrink-0" /><span className="truncate">Replying to {label(replyTo.author)}: {replyTo.text}</span>
            <button className="ml-auto shrink-0 hover:text-fg" title="Don't reply" onClick={() => setReplyTo(null)}><X className="size-3" /></button>
          </div>
        )}
        <div className="flex flex-wrap gap-1">
          {[...new Set(["@agents", ...Object.values(view.handles)])].map((h) => (
            <button key={h} onClick={() => mention(h)} className="flex items-center gap-0.5 rounded bg-accent/10 px-1.5 py-px text-[11px] text-accent hover:bg-accent/20" title={h === "@agents" ? "Wake every agent in the room" : `Wake ${h}`}>
              <Bot className="size-3" />{h.slice(1)}
            </button>
          ))}
          {[...new Set(["@everyone", ...Object.entries(view.people ?? {}).filter(([id]) => id !== view.me).map(([, h]) => h)])].map((h) => (
            <button key={h} onClick={() => mention(h)} className="flex items-center gap-0.5 rounded bg-hover px-1.5 py-px text-[11px] text-muted hover:text-fg" title={h === "@everyone" ? "Tell everyone in the room" : `Tell ${h.slice(1)}: they're notified`}>
              <AtSign className="size-3" />{h.slice(1)}
            </button>
          ))}
        </div>
        <div className="flex items-end gap-1.5">
          <Textarea
            rows={2}
            value={text}
            disabled={view.summary.removed}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); void send(); } }}
            onPaste={(e) => {
              // a pasted screenshot or file is shared, like a dropped one
              const pasted = [...e.clipboardData.files];
              if (!canShare || !pasted.length) return;
              e.preventDefault();
              void attach(pasted);
            }}
            placeholder={view.summary.removed ? "You're no longer in this room." : "Write to the room (Enter sends, Shift+Enter new line)"}
            className="min-h-0 flex-1 resize-none text-[13px]"
          />
          <Button variant="primary" loading={sending || sharing} disabled={!text.trim() || sharing} onClick={() => void send()} title="Send"><Send className="size-3.5" /></Button>
        </div>
      </div>
    </div>
  );
}

function Diff({ before, after }: { before: string; after: string }) {
  return (
    <div className="space-y-0.5 rounded border border-line bg-app p-1.5 font-mono text-[11.5px] break-words whitespace-pre-wrap">
      {before && <div className="bg-danger/10 text-danger line-through decoration-danger/50">{before}</div>}
      {after && <div className="bg-ok/10 text-ok">{after}</div>}
      {!before && !after && <div className="text-faint">(nothing)</div>}
    </div>
  );
}

function Card({ children, dim }: { children: ReactNode; dim?: boolean }) {
  return <div className={clsx("space-y-1.5 rounded-md border border-line bg-raised p-2.5", dim && "opacity-70")}>{children}</div>;
}

export function SuggestionsPanel({ room, canEdit, suggestions, onShow }: { room: string; canEdit: boolean; suggestions: Suggestion[]; onShow: (s: Suggestion) => void }) {
  const open = suggestions.filter((s) => s.state === "open");
  const done = suggestions.filter((s) => s.state !== "open").slice(-30).reverse();
  const decide = async (s: Suggestion, accept: boolean) => {
    try {
      await rooms.decide(room, s.id, accept);
    } catch (e) {
      toast.error(errMsg(e));
    }
  };
  return (
    <div className="h-full space-y-2 overflow-auto p-3">
      {open.length === 0 && <div className="py-6 text-center text-[12px] text-muted">No suggestions waiting. Agents propose changes here; you accept or reject them.</div>}
      {open.map((s) => (
        <Card key={s.id}>
          <div className="flex items-center gap-1.5 text-[11px] text-muted">
            {s.author.agent && <Bot className="size-3 text-accent" />}
            <span className="font-semibold text-fg">{label(s.author)}</span>
            <button className="truncate font-mono hover:text-fg hover:underline" onClick={() => onShow(s)}>{s.path}</button>
            <span className="ml-auto">{time(s.at)}</span>
          </div>
          {s.outdated && <Badge tone="warn">Out of date: the text changed since</Badge>}
          {!s.base && <Badge tone="info">New file</Badge>}
          {s.note && <div className="text-[12px]">{s.note}</div>}
          <Diff before={s.base} after={s.replacement} />
          {canEdit && (
            <div className="flex justify-end gap-1.5">
              <Button size="sm" onClick={() => void decide(s, false)}><X className="size-3" />Reject</Button>
              <Button size="sm" variant="primary" disabled={s.outdated} title={s.outdated ? "Ask its author to propose it again against the current text" : undefined} onClick={() => void decide(s, true)}><Check className="size-3" />Accept</Button>
            </div>
          )}
        </Card>
      ))}
      {done.length > 0 && <div className="pt-2 text-[11px] font-semibold tracking-wide text-faint uppercase">Decided</div>}
      {done.map((s) => (
        <Card key={s.id} dim>
          <div className="flex items-center gap-1.5 text-[11px] text-muted">
            <span className="font-semibold text-fg">{label(s.author)}</span>
            <span className="truncate font-mono">{s.path}</span>
            <Badge tone={s.state === "accepted" ? "ok" : "default"}>{s.state}</Badge>
            {s.decidedBy && <span className="ml-auto">by {label(s.decidedBy)}</span>}
          </div>
          <Diff before={s.base} after={s.replacement} />
        </Card>
      ))}
    </div>
  );
}

const KIND: Record<HistoryItem["kind"], string> = { suggestion: "accepted suggestion", edit: "agent edit", revert: "revert", newFile: "new file" };

/** Applied changes (each revertible while its text is untouched), or the room's activity: who
 *  shared, made, moved, deleted or took out what. */
export function HistoryPanel({ room, canEdit, history, activity }: { room: string; canEdit: boolean; history: HistoryItem[]; activity: Activity[] }) {
  const [show, setShow] = useState<"changes" | "activity">("changes");
  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex shrink-0 gap-0.5 border-b border-line px-2 py-1">
        {(["changes", "activity"] as const).map((k) => (
          <button key={k} onClick={() => setShow(k)} className={clsx("rounded px-2 py-0.5 text-[11.5px]", show === k ? "bg-hover text-fg" : "text-muted hover:text-fg")}>
            {k === "changes" ? "Changes" : "Who shared what"}
          </button>
        ))}
      </div>
      <div className="min-h-0 flex-1">
        {show === "changes" ? <ChangesList room={room} canEdit={canEdit} history={history} /> : <div className="h-full overflow-auto px-3 py-2"><ActivityFeed activity={activity} /></div>}
      </div>
    </div>
  );
}

function ChangesList({ room, canEdit, history }: { room: string; canEdit: boolean; history: HistoryItem[] }) {
  const revert = async (h: HistoryItem) => {
    try {
      await rooms.revert(room, h.id);
      toast.success("Reverted.");
    } catch (e) {
      toast.error(errMsg(e));
    }
  };
  return (
    <div className="h-full space-y-2 overflow-auto p-3">
      {history.length === 0 && <div className="py-6 text-center text-[12px] text-muted">Accepted suggestions and agents' edits show up here, and can be reverted later.</div>}
      {history.map((h) => (
        <Card key={h.id}>
          <div className="flex items-center gap-1.5 text-[11px] text-muted">
            {h.by.agent && <Bot className="size-3 text-accent" />}
            <span className="font-semibold text-fg">{label(h.by)}</span>
            <span>{KIND[h.kind]}</span>
            <span className="truncate font-mono">{h.path}</span>
            <span className="ml-auto">{time(h.at)}</span>
          </div>
          {h.approvedBy && <div className="text-[11px] text-muted">approved by {label(h.approvedBy)}</div>}
          <Diff before={h.before} after={h.after} />
          {canEdit && h.revertible && (
            <div className="flex justify-end">
              <Button size="sm" onClick={() => void revert(h)}><RotateCcw className="size-3" />Revert</Button>
            </div>
          )}
        </Card>
      ))}
    </div>
  );
}
