// Tickets state for the window: the boards, what each board's agent is doing, and a tick per board
// that bumps when its history changes (the tab reloads it). The sidebar lists the boards; the
// board picked there shows in the Tickets tab.
import { listen } from "@tauri-apps/api/event";
import { create } from "zustand";
import type { Project } from "../../lib/types";
import { openTab } from "../../store/tabs";
import { tickets, type Board, type JobView } from "./api";

interface TicketsState {
  boards: Board[];
  jobs: Record<string, JobView>;
  ticks: Record<string, number>;
  /** The board the Tickets tab shows. */
  selected: string | null;
  /** Board settings dialog: a board, "new", or closed. */
  editing: Board | "new" | null;
  refresh: () => Promise<void>;
  select: (id: string | null) => void;
  edit: (b: Board | "new" | null) => void;
}

export const useTickets = create<TicketsState>((set) => ({
  boards: [],
  jobs: {},
  ticks: {},
  selected: null,
  editing: null,
  refresh: async () => {
    const [boards, jobs] = await Promise.all([tickets.boards(), tickets.jobs()]);
    set({ boards, jobs });
  },
  select: (selected) => set({ selected }),
  edit: (editing) => set({ editing }),
}));

export function openTickets(boardId?: string) {
  if (boardId) useTickets.getState().select(boardId);
  openTab({ type: "tickets" }, "Tickets", "tickets");
}

/** The project whose code folder holds `dir` (the deepest one when they nest), like the backend's
 *  `projects::for_folder`. */
export function projectForFolder(projects: Project[], dir: string): Project | null {
  const norm = (p: string) => p.replace(/\\/g, "/").replace(/\/+$/, "");
  const d = norm(dir.trim());
  if (!d) return null;
  let best: { p: Project; len: number } | null = null;
  for (const p of projects) {
    for (const f of p.folders ?? []) {
      const n = norm(f);
      if (n && (d === n || d.startsWith(`${n}/`)) && (!best || n.length > best.len)) best = { p, len: n.length };
    }
  }
  return best?.p ?? null;
}

/** The board's project: the one it's set to, else the one its repository is in. */
export function boardProject(b: Board, projects: Project[]): { project: Project | null; detected: boolean } {
  if (b.projectId) return { project: projects.find((p) => p.id === b.projectId) ?? null, detected: false };
  return { project: projectForFolder(projects, b.repo), detected: true };
}

let started = false;
/** Listen to tickets events (once per window). */
export function startTickets() {
  if (started) return;
  started = true;
  void listen<{ board: string; job: JobView | null }>("tickets-job", (e) => {
    useTickets.setState((s) => {
      const jobs = { ...s.jobs };
      if (e.payload.job) jobs[e.payload.board] = e.payload.job;
      else delete jobs[e.payload.board];
      return { jobs };
    });
  });
  void listen<{ board: string }>("tickets-changed", (e) => {
    useTickets.setState((s) => ({ ticks: { ...s.ticks, [e.payload.board]: (s.ticks[e.payload.board] ?? 0) + 1 } }));
  });
}
