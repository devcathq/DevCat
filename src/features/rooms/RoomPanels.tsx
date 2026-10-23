// The room tab's side panels: the thread (with agent mentions), suggestions waiting for people, and
// the history of applied changes (each can be reverted while its text is untouched).
import clsx from "clsx";
import { AtSign, Bot, Check, CornerDownRight, Loader2, RotateCcw, Send, Square, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { toast } from "sonner";
import { Badge, Button, Textarea } from "../../components/ui";
import { errMsg } from "../../lib/api";
import { label, rooms, type HistoryItem, type Message, type RoomView, type Suggestion } from "./api";
import { useRooms } from "./store";

const time = (ms: number) => {
  const d = new Date(ms);
  const today = new Date().toDateString() === d.toDateString();
  return today ? d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }) : d.toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
};

/** Message text with @handles picked out. */
function Rich({ text, handles }: { text: string; handles: string[] }) {
  const parts = useMemo(() => {
    const known = new Set([...handles.map((h) => h.toLowerCase()), "@agents"]);
    return text.split(/(@[\w-]+)/g).map((p, i) => (known.has(p.toLowerCase()) ? <span key={i} className="rounded bg-accent/15 px-0.5 font-medium text-accent">{p}</span> : <span key={i}>{p}</span>));
  }, [text, handles]);
  return <div className="text-[13px] break-words whitespace-pre-wrap">{parts}</div>;
}

export function ThreadPanel({ room, view, messages }: { room: string; view: RoomView; messages: Message[] }) {
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const running = useRooms((s) => s.agents[room]) ?? view.agentRunning;
  const agentError = useRooms((s) => s.agentErrors[room]);
  const end = useRef<HTMLDivElement>(null);
  const handles = Object.values(view.handles);
  const byId = useMemo(() => new Map(messages.map((m) => [m.id, m])), [messages]);
  useEffect(() => end.current?.scrollIntoView({ block: "end" }), [messages.length]);
  const send = async () => {
    const t = text.trim();
    if (!t) return;
    setSending(true);
    try {
      await rooms.post(room, t);
      setText("");
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setSending(false);
    }
  };
  const mention = (h: string) => setText((t) => `${t}${t && !t.endsWith(" ") ? " " : ""}${h} `);
  const myHandle = view.handles[view.me];
  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="min-h-0 flex-1 space-y-2 overflow-auto p-3">
        {messages.length === 0 && <div className="py-8 text-center text-[12px] text-muted">Nothing said yet. Mention an agent to bring it in, e.g. <span className="font-mono">{myHandle ?? "@you-agent"}</span>.</div>}
        {messages.map((m) => {
          const reply = m.replyTo ? byId.get(m.replyTo) : undefined;
          const mine = m.author.member === view.me && !m.author.agent;
          return (
            <div key={m.id} className={clsx("rounded-md px-2.5 py-1.5", m.author.agent ? "border border-accent/25 bg-accent/5" : mine ? "bg-hover" : "bg-raised")}>
              <div className="mb-0.5 flex items-center gap-1.5 text-[11px] text-muted">
                {m.author.agent && <Bot className="size-3 text-accent" />}
                <span className={clsx("font-semibold", m.author.agent ? "text-accent" : "text-fg")}>{label(m.author)}</span>
                <span>{time(m.at)}</span>
              </div>
              {reply && (
                <div className="mb-1 flex items-start gap-1 text-[11px] text-faint">
                  <CornerDownRight className="mt-0.5 size-3 shrink-0" />
                  <span className="truncate">{label(reply.author)}: {reply.text}</span>
                </div>
              )}
              <Rich text={m.text} handles={handles} />
            </div>
          );
        })}
        <div ref={end} />
      </div>
      {running && (
        <div className="flex items-center gap-2 border-t border-line px-3 py-1.5 text-[12px] text-accent">
          <Loader2 className="size-3.5 animate-spin" />Your agent is working…
          <Button size="sm" variant="ghost" className="ml-auto" onClick={() => void rooms.agentStop(room)}><Square className="size-3" />Stop</Button>
        </div>
      )}
      {!running && agentError && <div className="border-t border-line px-3 py-1.5 text-[11px] text-danger">Your agent: {agentError}</div>}
      <div className="space-y-1.5 border-t border-line p-2">
        <div className="flex flex-wrap gap-1">
          {[...new Set(["@agents", ...handles])].map((h) => (
            <button key={h} onClick={() => mention(h)} className="flex items-center gap-0.5 rounded bg-hover px-1.5 py-px text-[11px] text-muted hover:text-fg" title={h === "@agents" ? "Wake every agent in the room" : `Wake ${h}`}>
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
            placeholder={view.summary.removed ? "You're no longer in this room." : "Write to the room (Enter sends, Shift+Enter new line)"}
            className="min-h-0 flex-1 resize-none text-[13px]"
          />
          <Button variant="primary" loading={sending} disabled={!text.trim()} onClick={() => void send()} title="Send"><Send className="size-3.5" /></Button>
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

export function HistoryPanel({ room, canEdit, history }: { room: string; canEdit: boolean; history: HistoryItem[] }) {
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
