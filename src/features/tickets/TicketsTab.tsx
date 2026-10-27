// Tickets: the board picked in the sidebar: the tickets in a column (pick some and let the agent
// fix them), what the agent is doing now, and what it did.
import { openUrl } from "@tauri-apps/plugin-opener";
import clsx from "clsx";
import { Bot, Brain, Eye, ExternalLink, GitBranch, ListTodo, Pencil, Play, Plus, RefreshCw, Square, Trash2 } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import { Badge, Button, Empty, ErrorBox, IconButton, Select, Spinner, Toolbar } from "../../components/ui";
import { errMsg } from "../../lib/api";
import { useConnections } from "../../store/connections";
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

  const fix = async (ids: string[]) => {
    try {
      await tickets.fix(board.id, ids);
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
            <IconButton title="Refresh" onClick={() => void load()}><RefreshCw className={clsx("size-3.5", loading && "animate-spin")} /></IconButton>
            {board.assigned !== "anyone" && <Badge tone="info">assigned: {ASSIGNED[board.assigned].toLowerCase()}</Badge>}
            {board.label && <Badge tone="info">label: {board.label}</Badge>}
            <Button size="sm" variant="primary" className="ml-auto" disabled={!ready || picked.size === 0} onClick={() => void fix([...picked])}>
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
                      {!busy && r && <Badge tone={OUTCOME[r.outcome].tone}>last try: {OUTCOME[r.outcome].label}</Badge>}
                    </div>
                  </div>
                  {t.url && <IconButton title="Open in the tracker" onClick={() => void openUrl(t.url)}><ExternalLink className="size-3.5" /></IconButton>}
                  <IconButton title="Fix this one with the agent" disabled={!ready || busy} onClick={() => void fix([t.id])}><Bot className="size-3.5" /></IconButton>
                </div>
              );
            })}
          </div>
        </section>
        <section className="flex w-[420px] shrink-0 flex-col">
          {job && <JobPanel board={board} job={job} />}
          <div className="flex h-8 shrink-0 items-center border-b border-line px-3 text-xs font-semibold tracking-wide text-muted uppercase">History</div>
          <div className="min-h-0 flex-1 overflow-auto">
            {history.length === 0 && <div className="p-4 text-[12px] text-faint">What the agent does shows up here.</div>}
            {history.map((r, i) => <HistoryItem key={`${r.ticketId}-${r.started}-${i}`} r={r} />)}
          </div>
        </section>
      </div>
    </div>
  );
}

function JobPanel({ board, job }: { board: Board; job: JobView }) {
  const logRef = useRef<HTMLDivElement>(null);
  useEffect(() => { logRef.current?.scrollTo({ top: logRef.current.scrollHeight }); }, [job.log.length]);
  return (
    <div className="flex max-h-[55%] min-h-40 flex-col border-b border-line bg-accent/5">
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

function HistoryItem({ r }: { r: RunRecord }) {
  const [open, setOpen] = useState(false);
  const o = OUTCOME[r.outcome] ?? OUTCOME.error;
  return (
    <div className="border-b border-line/60 px-3 py-2">
      <button className="flex w-full items-center gap-2 text-left" onClick={() => setOpen(!open)}>
        <Badge tone={o.tone}>{o.label}</Badge>
        <span className="shrink-0 font-mono text-[11px] text-muted">{r.key}</span>
        <span className="min-w-0 flex-1 truncate text-[12px]">{r.title}</span>
        <span className="shrink-0 text-[10px] text-faint">{when(r.finished)}</span>
      </button>
      {r.movedTo && <div className="mt-0.5 text-[11px] text-muted">Moved to {r.movedTo}</div>}
      {(open || r.outcome !== "fixed") && r.summary && <div className="selectable mt-1 text-[12px] whitespace-pre-wrap text-fg/80">{r.summary}</div>}
      {open && r.testing && <div className="selectable mt-1.5 text-[12px] whitespace-pre-wrap text-muted"><span className="font-medium text-fg/80">How to test: </span>{r.testing}</div>}
      <div className="mt-1 flex flex-wrap items-center gap-2 text-[11px] text-faint">
        {r.branch && <span className="flex items-center gap-1 font-mono"><GitBranch className="size-3" />{r.branch}</span>}
        {r.repos?.length ? <span>{r.repos.join(", ")}</span> : r.filesChanged > 0 && <span>{r.filesChanged} file{r.filesChanged === 1 ? "" : "s"}</span>}
        {r.costUsd != null && <span>${r.costUsd.toFixed(2)}</span>}
        {r.url && <button className="hover:text-fg" onClick={() => void openUrl(r.url)}>open ticket</button>}
      </div>
      {r.notes.map((n, i) => <div key={i} className="mt-1 text-[11px] text-warn">{n}</div>)}
    </div>
  );
}
