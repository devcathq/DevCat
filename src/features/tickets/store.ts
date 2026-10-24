// Tickets state for the window: the boards, what each board's agent is doing, and a tick per board
// that bumps when its history changes (the tab reloads it).
import { listen } from "@tauri-apps/api/event";
import { create } from "zustand";
import { openTab } from "../../store/tabs";
import { tickets, type Board, type JobView } from "./api";

interface TicketsState {
  boards: Board[];
  jobs: Record<string, JobView>;
  ticks: Record<string, number>;
  refresh: () => Promise<void>;
}

export const useTickets = create<TicketsState>((set) => ({
  boards: [],
  jobs: {},
  ticks: {},
  refresh: async () => {
    const [boards, jobs] = await Promise.all([tickets.boards(), tickets.jobs()]);
    set({ boards, jobs });
  },
}));

export function openTickets() {
  openTab({ type: "tickets" }, "Tickets", "tickets");
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
