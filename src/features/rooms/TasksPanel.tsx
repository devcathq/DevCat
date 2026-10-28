// The room's tasks: work for a person or an agent. Giving a task to someone else, or to an agent,
// says so in the thread (rooms.rs `announce_task`), which tells them and wakes a named agent.
import clsx from "clsx";
import { Bot, Check, CircleDot, Plus, Trash2, User } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { Button, Input, Select } from "../../components/ui";
import { errMsg } from "../../lib/api";
import { label, rooms, type Assignee, type RoomView, type Task, type TaskState } from "./api";

const STATE: Record<TaskState, string> = { open: "To do", doing: "Doing", done: "Done" };

/** "member:agent" ↔ Assignee, for a <select>. */
const key = (a?: Assignee | null) => (a ? `${a.member}:${a.agent ? 1 : 0}` : "");
const parse = (k: string): Assignee | null => (k ? { member: k.split(":")[0], agent: k.endsWith(":1") } : null);

function Who({ view, a }: { view: RoomView; a?: Assignee | null }) {
  if (!a) return <span className="text-faint">anyone</span>;
  const m = view.members.find((x) => x.id === a.member);
  const name = m ? (a.agent ? `${m.name}'s agent` : m.name) : "someone who left";
  return <span className="inline-flex items-center gap-0.5">{a.agent ? <Bot className="size-3 text-accent" /> : <User className="size-3" />}{name}</span>;
}

export function TasksPanel({ room, view, tasks, canEdit }: { room: string; view: RoomView; tasks: Task[]; canEdit: boolean }) {
  const [title, setTitle] = useState("");
  const [assign, setAssign] = useState("");
  const [busy, setBusy] = useState(false);
  const [showDone, setShowDone] = useState(false);
  const people = view.members.filter((m) => m.role !== "keeper");
  const options = people.flatMap((m) => [
    { k: key({ member: m.id, agent: false }), label: m.me ? `Me (${m.name})` : m.name },
    { k: key({ member: m.id, agent: true }), label: `${m.me ? "My" : `${m.name}'s`} agent (${view.handles[m.id] ?? ""})` },
  ]);
  const run = async (f: () => Promise<unknown>) => {
    try { await f(); } catch (e) { toast.error(errMsg(e)); }
  };
  const add = async () => {
    if (!title.trim()) return;
    setBusy(true);
    await run(async () => {
      await rooms.taskAdd(room, title, undefined, parse(assign));
      setTitle("");
    });
    setBusy(false);
  };
  const open = tasks.filter((t) => t.state !== "done");
  const done = tasks.filter((t) => t.state === "done");
  const row = (t: Task) => (
    <div key={t.id} className={clsx("group space-y-1 rounded-md border border-line bg-raised p-2", t.state === "done" && "opacity-60")}>
      <div className="flex items-start gap-2">
        <button disabled={!canEdit} title={t.state === "done" ? "Not done after all" : "Mark done"} onClick={() => void run(() => rooms.taskUpdate(room, t.id, { state: t.state === "done" ? "open" : "done" }))}
          className={clsx("mt-0.5 flex size-4 shrink-0 items-center justify-center rounded border", t.state === "done" ? "border-ok bg-ok/20 text-ok" : "border-line hover:border-accent")}>
          {t.state === "done" && <Check className="size-3" />}
        </button>
        <div className="min-w-0 flex-1">
          <div className={clsx("text-[13px] break-words", t.state === "done" && "line-through")}>{t.title}</div>
          {t.notes && <div className="text-[11.5px] break-words whitespace-pre-wrap text-muted">{t.notes}</div>}
          <div className="mt-0.5 flex flex-wrap items-center gap-x-2 text-[11px] text-muted">
            <Who view={view} a={t.assignee} />
            {t.state === "doing" && <span className="inline-flex items-center gap-0.5 text-accent"><CircleDot className="size-3" />doing</span>}
            <span className="text-faint">by {label(t.createdBy)}</span>
          </div>
        </div>
        {canEdit && (
          <button className="hidden text-faint group-hover:block hover:text-danger" title="Delete the task" onClick={() => void run(() => rooms.taskDelete(room, t.id))}><Trash2 className="size-3" /></button>
        )}
      </div>
      {canEdit && t.state !== "done" && (
        <div className="flex gap-1.5 pl-6">
          <Select className="h-6 flex-1 text-[11px]" value={key(t.assignee)} onChange={(e) => void run(() => rooms.taskUpdate(room, t.id, { assignee: parse(e.target.value) }))}>
            <option value="">For anyone</option>
            {options.map((o) => <option key={o.k} value={o.k}>{o.label}</option>)}
          </Select>
          <Select className="h-6 w-24 text-[11px]" value={t.state} onChange={(e) => void run(() => rooms.taskUpdate(room, t.id, { state: e.target.value as TaskState }))}>
            {(Object.keys(STATE) as TaskState[]).map((s) => <option key={s} value={s}>{STATE[s]}</option>)}
          </Select>
        </div>
      )}
    </div>
  );
  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="min-h-0 flex-1 space-y-2 overflow-auto p-3">
        {tasks.length === 0 && <div className="py-6 text-center text-[12px] text-muted">No tasks yet. Add one below, for a person or an agent: giving it to someone tells them in the thread.</div>}
        {open.map(row)}
        {done.length > 0 && (
          <button className="pt-1 text-[11px] font-semibold tracking-wide text-faint uppercase hover:text-muted" onClick={() => setShowDone((v) => !v)}>
            {showDone ? "Hide" : "Show"} {done.length} done
          </button>
        )}
        {showDone && done.map(row)}
      </div>
      {canEdit && (
        <div className="space-y-1.5 border-t border-line p-2">
          <Input value={title} onChange={(e) => setTitle(e.target.value)} placeholder="New task, e.g. Draft the introduction" onKeyDown={(e) => { if (e.key === "Enter") void add(); }} />
          <div className="flex gap-1.5">
            <Select className="flex-1" value={assign} onChange={(e) => setAssign(e.target.value)}>
              <option value="">For anyone</option>
              {options.map((o) => <option key={o.k} value={o.k}>{o.label}</option>)}
            </Select>
            <Button variant="primary" loading={busy} disabled={!title.trim()} onClick={() => void add()}><Plus className="size-3.5" />Add</Button>
          </div>
        </div>
      )}
    </div>
  );
}
