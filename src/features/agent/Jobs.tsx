// Scheduled DevCat Agent jobs of a folder, in the AI workspace sidebar (above its chats): each with
// its schedule, when it runs next and how its last run went; click for the last run's chat. The
// editor sets the prompt, the schedule, the connection and the permission mode.
import clsx from "clsx";
import { AlarmClock, Check, CircleAlert, Pause, Pencil, Play, Plus, Trash2 } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import { Button, Checkbox, Dialog, Field, IconButton, Input, Select, Spinner, Textarea } from "../../components/ui";
import { api, errMsg } from "../../lib/api";
import type { AgentJob, AgentJobSchedule, AgentPolicy } from "../../lib/types";
import { confirmDialog } from "../connections/dialogs";
import { useContextMenu } from "../connections/ContextMenu";
import { useProviders } from "../claude/providers";
import { openRun, scheduleText, useJobs, whenNext } from "./schedule";
import { POLICIES } from "./model";

export function AgentJobs({ cwd, providerId }: { cwd: string; providerId: string }) {
  const version = useJobs((s) => s.version);
  const [jobs, setJobs] = useState<AgentJob[] | null>(null);
  const [editing, setEditing] = useState<AgentJob | null>(null);
  const [now, setNow] = useState(Date.now());
  const menu = useContextMenu();
  useEffect(() => {
    let live = true;
    api.agentJobs(cwd).then((l) => live && setJobs(l), () => live && setJobs([]));
    return () => { live = false; };
  }, [cwd, version]);
  useEffect(() => { const t = setInterval(() => setNow(Date.now()), 30_000); return () => clearInterval(t); }, []);
  const save = async (job: AgentJob) => {
    try {
      await api.agentJobSave(job);
    } catch (e) {
      toast.error(errMsg(e));
    }
  };
  const runNow = (j: AgentJob) => void api.agentJobRun(j.id).catch((e) => toast.error(errMsg(e)));
  const remove = async (j: AgentJob) => {
    if (await confirmDialog({ title: `Delete "${j.name}"?`, message: "It won't run again. The chats of its runs stay in the list.", confirmLabel: "Delete", danger: true })) {
      await api.agentJobDelete(j.id).catch((e) => toast.error(errMsg(e)));
    }
  };
  const newJob = (): AgentJob => ({ id: "", name: "", cwd, providerId, prompt: "", policy: "plan", schedule: { kind: "daily", time: "09:00", days: [1, 2, 3, 4, 5] }, enabled: true, runs: [], nextMs: null });
  return (
    <div className="shrink-0 border-b border-line pb-1">
      <div className="flex h-6 items-center gap-1 px-3 text-[11px] text-faint">
        <AlarmClock className="size-3" /><span className="flex-1">Scheduled</span>
        <IconButton title="New scheduled job: a prompt that runs on a schedule in this folder" className="size-5" onClick={() => setEditing(newJob())}><Plus className="size-3.5" /></IconButton>
      </div>
      {jobs === null ? <div className="flex justify-center py-1"><Spinner className="size-3.5" /></div> : jobs.length === 0 ? (
        <button className="w-full px-3 pb-1 text-left text-[11.5px] text-faint hover:text-muted" onClick={() => setEditing(newJob())}>
          Run a prompt on a schedule here, like a morning report or a nightly check.
        </button>
      ) : jobs.map((j) => {
        const last = j.runs[0];
        const status = j.running ? "running" : !last ? null : last.end === "answered" ? "ok" : last.end ? "failed" : "running";
        return (
          <div key={j.id} className="group flex items-center gap-1 pr-1.5 hover:bg-hover"
            onContextMenu={(e) => menu.show(e, [
              { label: "Run now", icon: <Play className="size-3.5" />, onClick: () => runNow(j) },
              { label: "Edit…", icon: <Pencil className="size-3.5" />, onClick: () => setEditing(j) },
              { label: j.enabled ? "Pause" : "Resume", icon: <Pause className="size-3.5" />, onClick: () => void save({ ...j, enabled: !j.enabled }) },
              { label: "Delete…", icon: <Trash2 className="size-3.5" />, onClick: () => void remove(j) },
            ])}>
            <button className="flex min-w-0 flex-1 flex-col items-start px-3 py-1 text-left"
              title={last?.sessionId ? "Open the last run's chat (right-click for more)" : "Edit (right-click for more)"}
              onClick={() => (last?.sessionId ? void openRun(j.id, last.sessionId) : setEditing(j))}>
              <span className={clsx("flex w-full items-center gap-1.5 truncate text-[13px]", j.enabled ? "text-fg" : "text-faint")}>
                {status === "running" ? <Spinner className="size-3 shrink-0" /> : status === "ok" ? <Check className="size-3 shrink-0 text-ok" /> : status === "failed" ? <CircleAlert className="size-3 shrink-0 text-danger" /> : <AlarmClock className="size-3 shrink-0 text-faint" />}
                <span className="truncate">{j.name}</span>
              </span>
              <span className="w-full truncate text-[11px] text-faint">
                {scheduleText(j.schedule)} · {j.running ? "running" : !j.enabled ? "paused" : j.nextMs ? `next ${whenNext(j.nextMs, now)}` : ""}
                {last?.end && last.end !== "answered" && !j.running ? ` · last run ${last.end === "stopped" ? "stopped" : "failed"}` : ""}
              </span>
            </button>
            <IconButton title="Run now" className="size-5 opacity-0 group-hover:opacity-100" disabled={j.running} onClick={() => runNow(j)}><Play className="size-3" /></IconButton>
          </div>
        );
      })}
      {menu.element}
      {editing && <JobDialog job={editing} onClose={() => setEditing(null)} onSave={async (j) => { await save(j); setEditing(null); }} />}
    </div>
  );
}

const DAYS = ["M", "T", "W", "T", "F", "S", "S"];

function JobDialog({ job, onClose, onSave }: { job: AgentJob; onClose: () => void; onSave: (j: AgentJob) => Promise<void> }) {
  const [j, setJ] = useState(job);
  const [saving, setSaving] = useState(false);
  // the list as stored, filtered here (a selector returning a new array re-renders forever)
  const providers = useProviders((s) => s.providers);
  const agents = useMemo(() => providers.filter((p) => p.kind === "agent"), [providers]);
  const set = (p: Partial<AgentJob>) => setJ((x) => ({ ...x, ...p }));
  const setSchedule = (s: AgentJobSchedule) => set({ schedule: s });
  const s = j.schedule;
  const every = s.kind === "every" ? (s.minutes % 60 === 0 ? { n: s.minutes / 60, unit: 60 } : { n: s.minutes, unit: 1 }) : null;
  const ok = j.name.trim() && j.prompt.trim() && (s.kind === "every" ? s.minutes >= 5 : /^\d{2}:\d{2}$/.test(s.time));
  return (
    <Dialog open onClose={onClose} title={job.id ? `Edit "${job.name}"` : "New scheduled job"} width={560}
      footer={<>
        <Button variant="ghost" onClick={onClose}>Cancel</Button>
        <Button variant="primary" disabled={!ok} loading={saving} onClick={async () => { setSaving(true); await onSave(j); setSaving(false); }}>{job.id ? "Save" : "Add job"}</Button>
      </>}>
      <div className="flex flex-col gap-3">
        <Field label="Name"><Input autoFocus value={j.name} placeholder="Morning report" onChange={(e) => set({ name: e.target.value })} /></Field>
        <Field label="What should the agent do?" hint="Each run is a new chat in this folder with this as its message.">
          <Textarea rows={5} value={j.prompt} placeholder="Summarize yesterday's commits and open TODOs, and list anything that looks broken." onChange={(e) => set({ prompt: e.target.value })} />
        </Field>
        <div className="flex flex-wrap items-end gap-3">
          <Field label="When">
            <Select value={s.kind} onChange={(e) => setSchedule(e.target.value === "every" ? { kind: "every", minutes: 60 } : { kind: "daily", time: "09:00", days: [1, 2, 3, 4, 5] })}>
              <option value="daily">At a time of day</option>
              <option value="every">Every …</option>
            </Select>
          </Field>
          {s.kind === "daily" ? (
            <>
              <Field label="Time"><Input type="time" value={s.time} onChange={(e) => setSchedule({ ...s, time: e.target.value })} /></Field>
              <Field label="Days">
                <div className="flex h-7 items-center gap-0.5">
                  {DAYS.map((d, i) => {
                    const on = s.days.includes(i + 1);
                    return (
                      <button key={i} type="button" title={["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"][i]}
                        onClick={() => setSchedule({ ...s, days: on ? s.days.filter((x) => x !== i + 1) : [...s.days, i + 1].sort() })}
                        className={clsx("size-6 rounded text-[11px] font-medium", on ? "bg-accent text-accent-fg" : "bg-raised text-muted hover:text-fg")}>{d}</button>
                    );
                  })}
                </div>
              </Field>
            </>
          ) : every && (
            <Field label="Every">
              <div className="flex gap-1.5">
                <Input type="number" min={1} className="w-20" value={every.n} onChange={(e) => setSchedule({ kind: "every", minutes: Math.max(1, Number(e.target.value) || 1) * every.unit })} />
                <Select value={every.unit} onChange={(e) => setSchedule({ kind: "every", minutes: every.n * Number(e.target.value) })}>
                  <option value={1}>minutes</option>
                  <option value={60}>hours</option>
                </Select>
              </div>
            </Field>
          )}
        </div>
        {s.kind === "daily" && !s.days.length && <span className="-mt-2 text-[11px] text-faint">No day picked: it runs every day.</span>}
        {s.kind === "every" && s.minutes < 5 && <span className="-mt-2 text-[11px] text-danger">At most every 5 minutes.</span>}
        <div className="flex flex-wrap gap-3">
          <Field label="AI connection">
            <Select value={j.providerId} onChange={(e) => set({ providerId: e.target.value })}>
              {agents.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
            </Select>
          </Field>
          <Field label="Permission mode" hint={POLICIES.find((p) => p.id === j.policy)?.hint}>
            <Select value={j.policy} onChange={(e) => set({ policy: e.target.value as AgentPolicy })}>
              {POLICIES.map((p) => <option key={p.id} value={p.id}>{p.label}</option>)}
            </Select>
          </Field>
        </div>
        <p className="text-[11.5px] text-faint">
          Jobs run while DevCat is open; one that was due while it was closed runs once when it opens. When a run needs your OK,
          it waits in its chat (and on your phone), and you get a toast, or a phone notification if they're on.
        </p>
        <Checkbox label="On" checked={j.enabled} onChange={(v) => set({ enabled: v })} />
      </div>
    </Dialog>
  );
}
