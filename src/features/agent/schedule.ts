// Scheduled DevCat Agent jobs in the window (src-tauri/src/agent_jobs.rs): a version that moves
// when the backend says they changed (lists reload on it), a toast when one finishes or needs the
// user, and how a schedule reads. The app shell imports this once, so the toasts show anywhere.
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";
import { create } from "zustand";
import { api } from "../../lib/api";
import type { AgentJobSchedule } from "../../lib/types";
import { openClaudeWorkspace } from "../claude/open";

export const useJobs = create<{ version: number }>(() => ({ version: 0 }));

type JobEvent = { type: "changed" | "finished" | "waiting"; jobId?: string; name?: string; sessionId?: string | null; end?: string; message?: string };

void listen<JobEvent>("agent-jobs", (e) => {
  useJobs.setState((s) => ({ version: s.version + 1 }));
  const p = e.payload;
  if (p.type === "changed" || !p.jobId) return;
  const action = p.sessionId ? { label: "Open", onClick: () => void openRun(p.jobId!, p.sessionId!) } : undefined;
  if (p.type === "waiting") toast.warning(`${p.name}: needs you`, { description: p.message, action, duration: 20_000 });
  else if (p.end === "answered") toast.success(`${p.name} finished`, { description: p.message, action });
  else toast.error(`${p.name} didn't finish`, { description: p.message, action });
}).catch(() => {});

/** Open a run's chat in a tab of the job's folder. */
export async function openRun(jobId: string, sessionId: string) {
  const job = (await api.agentJobs(null)).find((j) => j.id === jobId);
  if (!job) return;
  // the tab takes the chat's title ("Morning report · Oct 23, 09:00")
  const title = (await api.agentSessions(job.cwd).catch(() => [])).find((s) => s.id === sessionId)?.title ?? job.name;
  openClaudeWorkspace(job.cwd, { providerId: job.providerId, sessionId, title });
}

const DAY = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/** "Every 30 min", "Every 2 h", "Daily at 09:00", "Weekdays at 09:00", "Mon, Thu at 18:30". */
export function scheduleText(s: AgentJobSchedule) {
  if (s.kind === "every") return s.minutes % 60 === 0 ? `Every ${s.minutes / 60 === 1 ? "hour" : `${s.minutes / 60} h`}` : `Every ${s.minutes} min`;
  const days = [...new Set(s.days)].sort();
  const which = !days.length || days.length === 7 ? "Daily" : days.join() === "1,2,3,4,5" ? "Weekdays" : days.join() === "6,7" ? "Weekends" : days.map((d) => DAY[d - 1]).join(", ");
  return `${which} at ${s.time}`;
}

/** "in 12 min", "in 3 h", "tomorrow 09:00", "Mon 09:00" for a time to come. */
export function whenNext(ms: number, now = Date.now()) {
  const mins = Math.round((ms - now) / 60_000);
  if (mins < 1) return "now";
  if (mins < 60) return `in ${mins} min`;
  const d = new Date(ms);
  const hm = d.toTimeString().slice(0, 5);
  if (mins < 12 * 60) return `in ${Math.round(mins / 60)} h (${hm})`;
  const today = new Date(now);
  const tomorrow = new Date(today.getFullYear(), today.getMonth(), today.getDate() + 1);
  if (d.toDateString() === today.toDateString()) return `today ${hm}`;
  if (d.toDateString() === tomorrow.toDateString()) return `tomorrow ${hm}`;
  return `${d.toLocaleDateString(undefined, { weekday: "short" })} ${hm}`;
}
