// Sidebar: the connected boards (Jira, Trello, Linear), with what the agent is doing on each.
// Clicking one shows it in the Tickets tab. In focus mode only the focused project's boards and
// the ones in no project are listed.
import clsx from "clsx";
import { Eye, ListTodo, Pencil, Plus, Square, Trash2 } from "lucide-react";
import { useEffect, useMemo } from "react";
import { toast } from "sonner";
import { Button, Spinner } from "../../components/ui";
import { errMsg } from "../../lib/api";
import { useConnections } from "../../store/connections";
import { useTabs } from "../../store/tabs";
import { useContextMenu } from "../connections/ContextMenu";
import { confirmDialog } from "../connections/dialogs";
import { TRACKERS, tickets, type Board } from "./api";
import { boardProject, openTickets, startTickets, useTickets } from "./store";

export default function TicketsPanel() {
  const boards = useTickets((s) => s.boards);
  const jobs = useTickets((s) => s.jobs);
  const selected = useTickets((s) => s.selected);
  const projects = useConnections((s) => s.projects);
  const focus = useConnections((s) => s.focusProjectId);
  const tabOpen = useTabs((s) => s.activeId === "tickets");
  const menu = useContextMenu();
  useEffect(() => {
    startTickets();
    void useTickets.getState().refresh().catch(() => {});
  }, []);

  const shown = useMemo(
    () => boards.map((b) => ({ b, ...boardProject(b, projects) })).filter(({ project }) => !focus || !project || project.id === focus),
    [boards, projects, focus],
  );

  const toggleWatch = async (b: Board) => {
    try {
      await tickets.save({ ...b, watch: !b.watch });
      await useTickets.getState().refresh();
    } catch (e) {
      toast.error(errMsg(e));
    }
  };
  const remove = async (b: Board) => {
    if (!(await confirmDialog({ title: `Disconnect ${b.name}?`, message: "Its token, webhook and history are removed from DevCat. Nothing changes on the board or in the repository.", confirmLabel: "Disconnect", danger: true }))) return;
    await tickets.remove(b.id);
    await useTickets.getState().refresh();
  };

  if (boards.length === 0) {
    return (
      <div className="space-y-3 p-4 text-[12px] text-muted">
        <p>Connect a Jira project, Trello board or Linear team, and the agent works through its tickets in your repository, with the project's shared memory.</p>
        <Button variant="primary" onClick={() => useTickets.getState().edit("new")}><Plus className="size-3.5" />Connect a board</Button>
      </div>
    );
  }
  return (
    <div className="h-full space-y-0.5 overflow-auto p-1.5">
      {shown.map(({ b, project }) => {
        const job = jobs[b.id];
        return (
          <button
            key={b.id}
            onClick={() => openTickets(b.id)}
            onContextMenu={(e) => {
              e.preventDefault();
              menu.show(e, [
                { label: "Board settings…", icon: <Pencil className="size-3.5" />, onClick: () => useTickets.getState().edit(b) },
                { label: b.watch ? "Stop watching" : `Watch ${b.pickFrom?.name ?? "the column"}`, icon: <Eye className="size-3.5" />, disabled: !b.repo || !b.doneTo || !b.pickFrom, onClick: () => void toggleWatch(b) },
                ...(job ? [{ label: "Stop the agent", icon: <Square className="size-3.5" />, onClick: () => void tickets.stop(b.id) }] : []),
                "separator",
                { label: "Disconnect…", icon: <Trash2 className="size-3.5" />, danger: true, onClick: () => void remove(b) },
              ]);
            }}
            title={job ? `Working on ${job.key ?? "a ticket"}${job.title ? `: ${job.title}` : ""}` : undefined}
            className={clsx("group flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left", b.id === selected && tabOpen ? "bg-hover text-fg" : "text-muted hover:bg-hover/60")}
          >
            <ListTodo className="size-4 shrink-0" style={{ color: project?.color ?? "var(--color-accent)" }} />
            <span className="min-w-0 flex-1">
              <span className="block truncate text-[13px]">{b.name}</span>
              <span className="block truncate text-[11px] text-faint">
                {job ? <span className="text-accent">{job.key ?? ""} {job.stage}</span> : <>{TRACKERS[b.kind]}{project ? ` · ${project.name}` : ""} · {b.pickFrom?.name ?? "?"} → {b.doneTo?.name ?? "?"}</>}
              </span>
            </span>
            {job ? <Spinner className="size-3.5 text-accent" /> : b.watch && <Eye className="size-3.5 text-ok" aria-label="Watching" />}
          </button>
        );
      })}
      {shown.length < boards.length && (
        <div className="px-2 py-1.5 text-[11px] text-faint">{boards.length - shown.length} board{boards.length - shown.length === 1 ? "" : "s"} of other projects hidden by focus</div>
      )}
      {menu.element}
    </div>
  );
}
