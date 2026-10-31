// Tickets: the board picked in the sidebar: the tickets in a column (pick some and let the agent
// fix them), what the agent is doing now, and what it did.
import { openUrl } from "@tauri-apps/plugin-opener";
import clsx from "clsx";
import { ArrowLeft, Bot, Brain, ChevronDown, ChevronRight, Eye, ExternalLink, GitBranch, ListTodo, MessageSquare, MessageSquareText, Pencil, Play, Plus, RefreshCw, Send, Square, Trash2, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import { Badge, Button, Dialog, Empty, ErrorBox, IconButton, Select, Spinner, Textarea, Toolbar, Spin } from "../../components/ui";
import { errMsg } from "../../lib/api";
import { useConnections } from "../../store/connections";
import { openClaudeWorkspace } from "../claude/open";
import { confirmDialog } from "../connections/dialogs";
import { ASSIGNED, TRACKERS, tickets, type Board, type JobView, type Outcome, type RunRecord, type Ticket } from "./api";
import { BoardDialog } from "./BoardDialog";
import { boardProject, openTickets, startTickets, useTickets } from "./store";

const OUTCOME: Record<Outcome, { label: string; tone: "ok" | "warn" | "danger" | "default" }> = {
  fixed: { label: "fixed", tone: "ok" },
  needs_info: { label: "needs a person", tone: "warn" },
  failed: { label: "couldn't fix", tone: "danger" },
  error: { label: "error", tone: "danger" },
  stopped: { label: "stopped", tone: "default" },
};

/** Outcomes where the agent waits on a person: their ticket gets a thread in the chat. */
const ASKS: Outcome[] = ["needs_info", "failed"];

/** Threads closed in the chat: board → ticket → the run it was closed at (a newer run opens it again). */
const DISMISSED = "devcat.tickets.dismissed";
const readDismissed = (board: string): Record<string, number> => {
  try { return JSON.parse(localStorage.getItem(DISMISSED) ?? "{}")[board] ?? {}; } catch { return {}; }
};
const writeDismissed = (board: string, m: Record<string, number>) => {
  let all: Record<string, Record<string, number>> = {};
  try { all = JSON.parse(localStorage.getItem(DISMISSED) ?? "{}"); } catch { /* start over */ }
  localStorage.setItem(DISMISSED, JSON.stringify({ ...all, [board]: m }));
};

type Panel = "working" | "chat" | "history";

/** Open a run's chat in the AI workspace (it can be continued there). */
const openRun = (r: RunRecord) =>
  r.sessionId && r.cwd && openClaudeWorkspace(r.cwd, { sessionId: r.sessionId, providerId: r.providerId, title: `${r.key} ${r.title}` });

const when = (ms: number) => new Date(ms).toLocaleString(undefined, { dateStyle: "short", timeStyle: "short" });

export default function TicketsTab() {
  const boards = useTickets((s) => s.boards);
  const jobs = useTickets((s) => s.jobs);
  const selected = useTickets((s) => s.selected);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    startTickets();
    void useTickets.getState().refresh().catch((e) => setError(errMsg(e)));
  }, []);
  useEffect(() => {
    if (!selected || !boards.some((b) => b.id === selected)) useTickets.getState().select(boards[0]?.id ?? null);
  }, [boards, selected]);
  const board = boards.find((b) => b.id === selected) ?? null;

  return (
    <div className="flex h-full min-h-0 flex-col">
      {error && <ErrorBox error={error} />}
      {board ? (
        <BoardView key={board.id} board={board} job={jobs[board.id] ?? null} onEdit={() => useTickets.getState().edit(board)} />
      ) : (
        <Empty icon={<ListTodo className="size-8" />} title="Let the agent work through your tickets">
          <p className="mb-3">
            Connect a Jira project, Trello board or Linear team. The agent takes tickets from a column (say, To Do), fixes each one on its own branch,
            comments on the ticket and moves it on (say, to Testing). Results can go to Slack too.
          </p>
          <Button variant="primary" onClick={() => useTickets.getState().edit("new")}><Plus className="size-3.5" />Connect a board</Button>
        </Empty>
      )}
    </div>
  );
}

/** The board settings dialog, wherever it was opened from (sidebar or tab). Mounted once by AppShell. */
export function TicketsDialog() {
  const editing = useTickets((s) => s.editing);
  if (!editing) return null;
  return (
    <BoardDialog
      board={editing === "new" ? null : editing}
      onClose={() => useTickets.getState().edit(null)}
      onSaved={(b) => {
        useTickets.getState().edit(null);
        openTickets(b.id);
        void useTickets.getState().refresh();
      }}
    />
  );
}

function BoardView({ board, job, onEdit }: { board: Board; job: JobView | null; onEdit: () => void }) {
  const [column, setColumn] = useState(board.pickFrom?.id ?? "");
  const [list, setList] = useState<Ticket[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [history, setHistory] = useState<RunRecord[]>([]);
  const tick = useTickets((s) => s.ticks[board.id] ?? 0);
  // The columns we know of: the board's own (refetched from the tracker when listing).
  const columns = useMemo(() => {
    const all = [board.pickFrom, board.working, board.doneTo, board.failedTo].filter((c): c is NonNullable<typeof c> => !!c);
    return all.filter((c, i) => all.findIndex((x) => x.id === c.id) === i);
  }, [board]);

  const load = useCallback(async () => {
    if (!column) return;
    setLoading(true);
    setError(null);
    try {
      setList(await tickets.list(board.id, column));
      setPicked(new Set());
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setLoading(false);
    }
  }, [board.id, column]);
  useEffect(() => { void load(); }, [load]);
  useEffect(() => { void tickets.history(board.id).then(setHistory).catch(() => {}); }, [board.id, tick]);
  // A finished ticket has moved: refresh the column.
  useEffect(() => { if (tick) void load(); }, [tick]); // eslint-disable-line react-hooks/exhaustive-deps

  const last = useMemo(() => {
    const m = new Map<string, RunRecord>();
    for (const r of history) if (!m.has(r.ticketId)) m.set(r.ticketId, r);
    return m;
  }, [history]);

  // The chat: a thread per ticket the agent asked about (its last run needs a person), or that it
  // works on after an answer; closed threads stay closed until the agent asks again.
  const [dismissed, setDismissed] = useState(() => readDismissed(board.id));
  const busyOn = (id: string) => !!job && (job.key === id || job.key === last.get(id)?.key || job.queue.includes(id));
  const threads = useMemo(
    () => [...last.values()].filter((r) => (ASKS.includes(r.outcome) && dismissed[r.ticketId] !== r.finished) || (r.reply && busyOn(r.ticketId))),
    [last, dismissed, job], // eslint-disable-line react-hooks/exhaustive-deps
  );
  const waiting = threads.filter((r) => !busyOn(r.ticketId)).length;
  const [panel, setPanel] = useState<Panel>(() => (job ? "working" : "chat"));
  const [thread, setThread] = useState<string | null>(null);
  const openThread = (ticketId: string) => { setThread(ticketId); setPanel("chat"); };
  const dismiss = (r: RunRecord) => {
    const m = { ...dismissed, [r.ticketId]: r.finished };
    setDismissed(m);
    writeDismissed(board.id, m);
    if (thread === r.ticketId) setThread(null);
  };
  // Work started (a fix or an answer): show it, unless the person is in a thread.
  const working = !!job;
  useEffect(() => { if (working && !thread) setPanel("working"); }, [working]); // eslint-disable-line react-hooks/exhaustive-deps

  // Tickets to fix with a prompt of their own (the dialog is open).
  const [asking, setAsking] = useState<Ticket[] | null>(null);
  const fix = async (ids: string[], prompt?: string | null) => {
    try {
      await tickets.fix(board.id, ids, prompt);
      toast.success(ids.length === 1 ? "The agent is on it" : `Queued ${ids.length} tickets`);
      setPicked(new Set());
    } catch (e) {
      toast.error(errMsg(e));
    }
  };
  const toggleWatch = async () => {
    try {
      await tickets.save({ ...board, watch: !board.watch });
      await useTickets.getState().refresh();
    } catch (e) {
      toast.error(errMsg(e));
    }
  };
  const remove = async () => {
    if (!(await confirmDialog({ title: `Disconnect ${board.name}?`, message: "Its token, webhook and history are removed from DevCat. Nothing changes on the board or in the repository.", confirmLabel: "Disconnect", danger: true }))) return;
    await tickets.remove(board.id);
    await useTickets.getState().refresh();
  };

  const toggle = (id: string) => setPicked((s) => { const n = new Set(s); if (n.has(id)) n.delete(id); else n.add(id); return n; });
  const ready = !!board.repo && !!board.doneTo;
  const projects = useConnections((s) => s.projects);
  const { project, detected } = boardProject(board, projects);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <Toolbar className="gap-2">
        <span className="truncate font-medium">{board.name}</span>
        <Badge>{TRACKERS[board.kind]}</Badge>
        <span className="truncate text-[12px] text-muted">
          {board.pickFrom?.name ?? "?"}{board.working ? ` → ${board.working.name}` : ""} → {board.doneTo?.name ?? "?"}
        </span>
        <span
          className="flex shrink-0 items-center gap-1 text-[11px] text-muted"
          title={project
            ? `The agent uses the shared memory and connections of the project ${project.name}${detected ? " (its code folder holds the repository)" : ""}, like a chat there`
            : "The agent uses the repository's shared memory (set a project in the board settings for its memory and connections)"}
        >
          <Brain className="size-3" style={project?.color ? { color: project.color } : undefined} />
          {project ? project.name : "repository memory"}{detected && project ? " (detected)" : ""}
        </span>
        <div className="ml-auto flex items-center gap-1">
          <Button size="sm" variant={board.watch ? "primary" : "default"} onClick={() => void toggleWatch()} disabled={!ready || !board.pickFrom} title={`Check ${board.pickFrom?.name ?? "the column"} every ${board.intervalMin} min and work on new tickets`}>
            <Eye className="size-3" />{board.watch ? `Watching (${board.intervalMin} min)` : "Watch"}
          </Button>
          <IconButton title="Board settings" onClick={onEdit}><Pencil className="size-3.5" /></IconButton>
          <IconButton title="Disconnect" onClick={() => void remove()}><Trash2 className="size-3.5" /></IconButton>
        </div>
      </Toolbar>
      {!ready && (
        <div className="m-3 rounded-md border border-warn/30 bg-warn/10 p-2.5 text-[12px] text-warn">
          Finish the board's settings: {!board.repo && "the repository the agent works in"}{!board.repo && !board.doneTo && ", "}{!board.doneTo && "where fixed tickets go"}.{" "}
          <button className="underline" onClick={onEdit}>Open settings</button>
        </div>
      )}
      <div className="flex min-h-0 flex-1">
        <section className="flex min-w-0 flex-1 flex-col border-r border-line">
          <div className="flex h-9 shrink-0 items-center gap-2 border-b border-line px-3">
            <Select className="w-48" value={column} onChange={(e) => setColumn(e.target.value)}>
              {columns.map((c) => <option key={c.id} value={c.id}>{c.name}</option>)}
            </Select>
            <IconButton title="Refresh" onClick={() => void load()}><Spin icon={RefreshCw} spinning={loading} className="size-3.5" /></IconButton>
            {board.assigned !== "anyone" && <Badge tone="info">assigned: {ASSIGNED[board.assigned].toLowerCase()}</Badge>}
            {board.label && <Badge tone="info">label: {board.label}</Badge>}
            <Button size="sm" className="ml-auto" disabled={!ready || picked.size === 0} onClick={() => setAsking((list ?? []).filter((t) => picked.has(t.id)))} title="Write what the agent should do for these tickets, instead of the board's default prompt">
              <MessageSquareText className="size-3" />With a prompt…
            </Button>
            <Button size="sm" variant="primary" disabled={!ready || picked.size === 0} onClick={() => void fix([...picked])} title="Fix them with the board's default prompt">
              <Play className="size-3" />Fix {picked.size || ""} with the agent
            </Button>
          </div>
          <div className="min-h-0 flex-1 overflow-auto">
            {error && <ErrorBox error={error} />}
            {list === null && loading && <div className="flex justify-center p-6"><Spinner /></div>}
            {list && list.length === 0 && <Empty title="No tickets here">Nothing in this column{board.assigned !== "anyone" ? ` assigned to ${board.assigned === "me" ? "you" : "you or nobody"}` : ""}{board.label ? ` labelled “${board.label}”` : ""}.</Empty>}
            {list?.map((t) => {
              const r = last.get(t.id);
              const busy = job?.key === t.key || job?.queue.includes(t.id);
              return (
                <div key={t.id} className={clsx("flex items-start gap-2.5 border-b border-line/60 px-3 py-2 hover:bg-hover/40", picked.has(t.id) && "bg-accent/5")}>
                  <input type="checkbox" className="mt-1 accent-[var(--color-accent)]" checked={picked.has(t.id)} onChange={() => toggle(t.id)} disabled={busy} />
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2">
                      <span className="shrink-0 font-mono text-[11px] text-muted">{t.key}</span>
                      <span className="truncate text-[13px]">{t.title}</span>
                    </div>
                    {t.description && <div className="mt-0.5 line-clamp-2 text-[12px] text-faint">{t.description}</div>}
                    <div className="mt-1 flex flex-wrap items-center gap-1">
                      {t.labels.map((l) => <Badge key={l}>{l}</Badge>)}
                      {t.assignees.length > 0 && <span className="text-[11px] text-muted">{t.assignees.join(", ")}</span>}
                      {busy && <Badge tone="accent">{job?.key === t.key ? "agent working" : "queued"}</Badge>}
                      {!busy && r && (
                        <button
                          title={ASKS.includes(r.outcome) ? "Answer the agent in the chat" : "See what the agent did"}
                          onClick={() => (ASKS.includes(r.outcome) ? openThread(t.id) : setPanel("history"))}
                        >
                          <Badge tone={OUTCOME[r.outcome].tone}>last try: {OUTCOME[r.outcome].label}{ASKS.includes(r.outcome) ? " · answer" : ""}</Badge>
                        </button>
                      )}
                    </div>
                  </div>
                  {t.url && <IconButton title="Open in the tracker" onClick={() => void openUrl(t.url)}><ExternalLink className="size-3.5" /></IconButton>}
                  <IconButton title="Fix this one with a prompt of its own…" disabled={!ready || busy} onClick={() => setAsking([t])}><MessageSquareText className="size-3.5" /></IconButton>
                  <IconButton title="Fix this one with the agent (the board's default prompt)" disabled={!ready || busy} onClick={() => void fix([t.id])}><Bot className="size-3.5" /></IconButton>
                </div>
              );
            })}
          </div>
        </section>
        <section className="flex w-[420px] shrink-0 flex-col">
          <div className="flex h-9 shrink-0 items-end gap-1 border-b border-line px-2">
            {([
              ["working", "Working", job ? <Spinner key="s" className="size-3 text-accent" /> : null],
              ["chat", "Chat", waiting ? <Badge key="b" tone="warn">{waiting}</Badge> : null],
              ["history", "History", null],
            ] as const).map(([id, label, extra]) => (
              <button
                key={id}
                onClick={() => { setPanel(id); if (id === "chat") setThread(null); }}
                className={clsx("-mb-px flex h-8 items-center gap-1.5 border-b-2 px-2.5 text-[12px]", panel === id ? "border-accent text-fg" : "border-transparent text-muted hover:text-fg")}
              >
                {label}{extra}
              </button>
            ))}
          </div>
          {panel === "working" && (job
            ? <JobPanel board={board} job={job} />
            : <div className="p-4 text-[12px] text-faint">The agent isn't working on anything. Pick tickets and fix them with the agent, or answer its questions in the chat.</div>)}
          {panel === "chat" && (thread
            ? <Thread
                key={thread}
                board={board}
                runs={history.filter((r) => r.ticketId === thread).reverse()}
                busy={busyOn(thread)}
                ready={ready}
                onBack={() => setThread(null)}
                onSent={() => setPanel("working")}
              />
            : <ThreadList threads={threads} busyOn={busyOn} onOpen={setThread} onDismiss={dismiss} />)}
          {panel === "history" && (
            <div className="min-h-0 flex-1 overflow-auto">
              {history.length === 0 && <div className="p-4 text-[12px] text-faint">What the agent does shows up here.</div>}
              {history.map((r, i) => <HistoryItem key={`${r.ticketId}-${r.started}-${i}`} r={r} onAnswer={ASKS.includes(r.outcome) && last.get(r.ticketId) === r ? () => openThread(r.ticketId) : undefined} />)}
            </div>
          )}
        </section>
      </div>
      {asking && (
        <FixWithPrompt
          board={board}
          list={asking}
          onClose={() => setAsking(null)}
          onFix={(prompt) => { const ids = asking.map((t) => t.id); setAsking(null); void fix(ids, prompt); }}
        />
      )}
    </div>
  );
}

/** Fix tickets with a prompt of their own: it replaces the board's default prompt for them (the
 *  ticket's text, comments and images still go to the agent). Starts from the default prompt. */
function FixWithPrompt({ board, list, onClose, onFix }: { board: Board; list: Ticket[]; onClose: () => void; onFix: (prompt: string | null) => void }) {
  const [text, setText] = useState(board.instructions);
  const changed = text.trim() !== board.instructions.trim();
  const what = list.length === 1 ? list[0].key : `${list.length} tickets`;
  return (
    <Dialog
      open
      onClose={onClose}
      title={`Fix ${what} with a prompt`}
      width={620}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" onClick={() => onFix(changed && text.trim() ? text : null)}>
            <Play className="size-3" />{changed && text.trim() ? "Fix with this prompt" : "Fix with the default prompt"}
          </Button>
        </>
      }
    >
      <div className="space-y-2">
        {list.length > 1 && <div className="truncate text-[12px] text-muted">{list.map((t) => t.key).join(", ")}</div>}
        {list.length === 1 && <div className="truncate text-[13px]"><span className="font-mono text-[11px] text-muted">{list[0].key}</span> {list[0].title}</div>}
        <Textarea
          autoFocus
          rows={8}
          className="w-full"
          value={text}
          onChange={(e) => setText(e.target.value)}
          placeholder="What the agent should do for this ticket: the approach, files to change or leave alone, how to test…"
        />
        <p className="text-[11px] text-muted">
          Replaces the board's default prompt for {list.length === 1 ? "this ticket" : "these tickets"}, this time only. The ticket's text, comments and
          images still go to the agent, and DevCat still moves and comments on the ticket as the board says.
          {board.instructions.trim() && changed && <> <button className="underline hover:text-fg" onClick={() => setText(board.instructions)}>Back to the default prompt</button></>}
        </p>
      </div>
    </Dialog>
  );
}

function JobPanel({ board, job }: { board: Board; job: JobView }) {
  const logRef = useRef<HTMLDivElement>(null);
  useEffect(() => { logRef.current?.scrollTo({ top: logRef.current.scrollHeight }); }, [job.log.length]);
  return (
    <div className="flex min-h-0 flex-1 flex-col bg-accent/5">
      <div className="flex items-center gap-2 px-3 pt-2">
        <Spinner className="size-3.5 text-accent" />
        <span className="min-w-0 flex-1 truncate text-[13px]">
          <span className="font-mono text-[11px] text-muted">{job.key}</span> {job.title}
        </span>
        <Button size="sm" variant="danger" disabled={job.stopping} onClick={() => void tickets.stop(board.id)}>
          <Square className="size-3" />{job.stopping ? "Stopping…" : "Stop"}
        </Button>
      </div>
      <div className="px-3 pt-0.5 text-[11px] text-muted">
        {job.stage}{job.queue.length ? ` · ${job.queue.length} more queued` : ""}
      </div>
      <div ref={logRef} className="selectable mx-3 my-2 min-h-0 flex-1 overflow-auto rounded border border-line bg-app p-2 font-mono text-[11px] leading-relaxed text-muted">
        {job.log.map((l, i) => <div key={i} className={clsx("truncate", l.startsWith("—") && "text-fg")}>{l}</div>)}
      </div>
    </div>
  );
}

/** The tickets the agent asked about, newest first. */
function ThreadList({ threads, busyOn, onOpen, onDismiss }: {
  threads: RunRecord[]; busyOn: (id: string) => boolean; onOpen: (id: string) => void; onDismiss: (r: RunRecord) => void;
}) {
  if (threads.length === 0) {
    return <div className="p-4 text-[12px] text-faint">When the agent needs a person (a question, a decision, something it couldn't do), the ticket shows up here and you can answer it.</div>;
  }
  return (
    <div className="min-h-0 flex-1 overflow-auto">
      {threads.map((r) => {
        const busy = busyOn(r.ticketId);
        return (
          <div key={r.ticketId} className="group flex items-start gap-2 border-b border-line/60 px-3 py-2 hover:bg-hover/40">
            <button className="min-w-0 flex-1 text-left" onClick={() => onOpen(r.ticketId)}>
              <div className="flex items-center gap-2">
                {busy ? <Badge tone="accent">agent working</Badge> : <Badge tone={OUTCOME[r.outcome].tone}>{OUTCOME[r.outcome].label}</Badge>}
                <span className="shrink-0 font-mono text-[11px] text-muted">{r.key}</span>
                <span className="min-w-0 flex-1 truncate text-[12px]">{r.title}</span>
                <span className="shrink-0 text-[10px] text-faint">{when(r.finished)}</span>
              </div>
              {r.summary && <div className="mt-1 line-clamp-2 text-[12px] text-muted">{r.summary}</div>}
            </button>
            <IconButton className="opacity-0 group-hover:opacity-100" title="Close this thread (it opens again if the agent asks again)" onClick={() => onDismiss(r)}><X className="size-3.5" /></IconButton>
          </div>
        );
      })}
    </div>
  );
}

/** One ticket's conversation: each run's answer from the agent, and the person's answers that
 *  started the next ones. Answering sends the agent back to the ticket, in the same chat. */
function Thread({ board, runs, busy, ready, onBack, onSent }: {
  board: Board; runs: RunRecord[]; busy: boolean; ready: boolean; onBack: () => void; onSent: () => void;
}) {
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const endRef = useRef<HTMLDivElement>(null);
  useEffect(() => { endRef.current?.scrollIntoView({ block: "end" }); }, [runs.length, busy]);
  const r = runs[runs.length - 1];
  if (!r) return null;
  const send = async () => {
    if (!text.trim() || sending) return;
    setSending(true);
    try {
      await tickets.reply(board.id, r.ticketId, text);
      setText("");
      toast.success("The agent goes on with your answer");
      onSent();
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setSending(false);
    }
  };
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex shrink-0 items-center gap-2 border-b border-line px-2 py-1.5">
        <IconButton title="All threads" onClick={onBack}><ArrowLeft className="size-3.5" /></IconButton>
        <span className="shrink-0 font-mono text-[11px] text-muted">{r.key}</span>
        <span className="min-w-0 flex-1 truncate text-[12px]">{r.title}</span>
        {r.url && <IconButton title="Open in the tracker" onClick={() => void openUrl(r.url)}><ExternalLink className="size-3.5" /></IconButton>}
      </div>
      <div className="selectable min-h-0 flex-1 space-y-2.5 overflow-auto p-3">
        {runs.map((x, i) => (
          <div key={`${x.started}-${i}`} className="space-y-2.5">
            {x.reply && (
              <div className="ml-8 rounded-lg bg-accent/10 px-3 py-2 text-[12px] whitespace-pre-wrap">{x.reply}</div>
            )}
            <div className="mr-4 rounded-lg border border-line bg-panel px-3 py-2">
              <div className="mb-1 flex items-center gap-2 text-[11px] text-faint">
                <Bot className="size-3" />
                <Badge tone={(OUTCOME[x.outcome] ?? OUTCOME.error).tone}>{(OUTCOME[x.outcome] ?? OUTCOME.error).label}</Badge>
                <span className="ml-auto">{when(x.finished)}</span>
              </div>
              {x.summary && <div className="text-[12px] whitespace-pre-wrap text-fg/85">{x.summary}</div>}
              {x.testing && <div className="mt-1.5 text-[12px] whitespace-pre-wrap text-muted"><span className="font-medium text-fg/80">How to test: </span>{x.testing}</div>}
              {x.notes.map((n, j) => <div key={j} className="mt-1 text-[11px] text-warn">{n}</div>)}
              <div className="mt-1.5 flex flex-wrap items-center gap-2 text-[11px] text-faint">
                {x.movedTo && <span>Moved to {x.movedTo}</span>}
                {x.branch && <span className="flex items-center gap-1 font-mono"><GitBranch className="size-3" />{x.branch}</span>}
                {x.sessionId && x.cwd && (
                  <button className="flex items-center gap-1 hover:text-fg" title="Open the agent's chat in the AI workspace" onClick={() => openRun(x)}>
                    <MessageSquare className="size-3" />open in AI workspace
                  </button>
                )}
              </div>
            </div>
          </div>
        ))}
        {busy && (
          <div className="mr-4 flex items-center gap-2 rounded-lg border border-line px-3 py-2 text-[12px] text-muted">
            <Spinner className="size-3 text-accent" />The agent is working on it…
          </div>
        )}
        <div ref={endRef} />
      </div>
      <div className="shrink-0 border-t border-line p-2">
        <Textarea
          rows={3}
          className="w-full"
          value={text}
          disabled={busy || !ready}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => { if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) { e.preventDefault(); void send(); } }}
          placeholder={busy ? "The agent is working on this ticket…" : "Answer the agent: it goes on with the ticket in the same chat, on its branch"}
        />
        <div className="mt-1.5 flex items-center gap-2">
          <span className="text-[11px] text-faint">⌘↵ to send · stays in DevCat, not posted to the tracker</span>
          <Button size="sm" variant="primary" className="ml-auto" disabled={!text.trim() || busy || sending || !ready} onClick={() => void send()}>
            <Send className="size-3" />Send
          </Button>
        </div>
      </div>
    </div>
  );
}

/** A run: clicking it opens the agent's chat in the AI workspace (the chevron shows the details). */
function HistoryItem({ r, onAnswer }: { r: RunRecord; onAnswer?: () => void }) {
  const [open, setOpen] = useState(false);
  const o = OUTCOME[r.outcome] ?? OUTCOME.error;
  const canOpen = !!(r.sessionId && r.cwd);
  return (
    <div className="border-b border-line/60 px-3 py-2 hover:bg-hover/40">
      <div className="flex w-full items-center gap-2">
        <button className="shrink-0 text-faint hover:text-fg" title={open ? "Hide details" : "Show details"} onClick={() => setOpen(!open)}>
          {open ? <ChevronDown className="size-3.5" /> : <ChevronRight className="size-3.5" />}
        </button>
        <button
          className="flex min-w-0 flex-1 items-center gap-2 text-left"
          title={canOpen ? "Open the agent's chat in the AI workspace" : "Show details"}
          onClick={() => (canOpen ? openRun(r) : setOpen(!open))}
        >
          <Badge tone={o.tone}>{o.label}</Badge>
          <span className="shrink-0 font-mono text-[11px] text-muted">{r.key}</span>
          <span className="min-w-0 flex-1 truncate text-[12px]">{r.title}</span>
          <span className="shrink-0 text-[10px] text-faint">{when(r.finished)}</span>
        </button>
      </div>
      {r.movedTo && <div className="mt-0.5 text-[11px] text-muted">Moved to {r.movedTo}</div>}
      {r.reply && <div className="mt-0.5 truncate text-[11px] text-muted" title={r.reply}><MessageSquareText className="mr-1 inline size-3" />Your answer: {r.reply}</div>}
      {r.prompt && (open
        ? <div className="selectable mt-1 text-[12px] whitespace-pre-wrap text-muted"><span className="font-medium text-fg/80">Prompt: </span>{r.prompt}</div>
        : <div className="mt-0.5 truncate text-[11px] text-muted" title={r.prompt}><MessageSquareText className="mr-1 inline size-3" />{r.prompt}</div>)}
      {r.summary && (open
        ? <div className="selectable mt-1 text-[12px] whitespace-pre-wrap text-fg/80">{r.summary}</div>
        : <div className="mt-0.5 line-clamp-2 text-[12px] text-fg/70">{r.summary}</div>)}
      {open && r.testing && <div className="selectable mt-1.5 text-[12px] whitespace-pre-wrap text-muted"><span className="font-medium text-fg/80">How to test: </span>{r.testing}</div>}
      <div className="mt-1 flex flex-wrap items-center gap-2 text-[11px] text-faint">
        {r.branch && <span className="flex items-center gap-1 font-mono"><GitBranch className="size-3" />{r.branch}</span>}
        {r.repos?.length ? <span>{r.repos.join(", ")}</span> : r.filesChanged > 0 && <span>{r.filesChanged} file{r.filesChanged === 1 ? "" : "s"}</span>}
        {r.costUsd != null && <span>${r.costUsd.toFixed(2)}</span>}
        {r.url && <button className="hover:text-fg" onClick={() => void openUrl(r.url)}>open ticket</button>}
        {canOpen && (
          <button className="flex items-center gap-1 hover:text-fg" title="Open the agent's chat in the AI workspace (you can continue it there)" onClick={() => openRun(r)}>
            <MessageSquare className="size-3" />open chat
          </button>
        )}
        {onAnswer && <button className="flex items-center gap-1 text-warn hover:text-fg" onClick={onAnswer}><Send className="size-3" />answer</button>}
      </div>
      {r.notes.map((n, i) => <div key={i} className="mt-1 text-[11px] text-warn">{n}</div>)}
    </div>
  );
}
