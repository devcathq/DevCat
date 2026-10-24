// Rooms state for the window: the list, join requests waiting for this DevCat's user, which rooms'
// agents are working, and the events that change them.
import { listen } from "@tauri-apps/api/event";
import { create } from "zustand";
import { openTab } from "../../store/tabs";
import { rooms, type JoinRequest, type RoomSummary } from "./api";

interface RoomsState {
  id: string | null;
  list: RoomSummary[];
  pending: JoinRequest[];
  /** Rooms whose agent (this DevCat's) is working. */
  agents: Record<string, boolean>;
  agentErrors: Record<string, string | undefined>;
  dialog: boolean;
  /** Bumped on every change of a room, by room id (tabs refresh on it). */
  ticks: Record<string, number>;
  show: (open?: boolean) => void;
  refresh: (start?: boolean) => Promise<void>;
}

export const useRooms = create<RoomsState>((set) => ({
  id: null,
  list: [],
  pending: [],
  agents: {},
  agentErrors: {},
  dialog: false,
  ticks: {},
  show: (open = true) => {
    set({ dialog: open });
    if (open) void useRooms.getState().refresh();
  },
  refresh: async (start = true) => {
    const i = await rooms.info(start);
    set({ id: i.id, list: i.rooms, pending: i.pending });
  },
}));

const NAME_KEY = "devcat.rooms.myName";
/** The name this user goes by in rooms (asked once, offered again). */
export const myName = () => localStorage.getItem(NAME_KEY) ?? "";
export const setMyName = (n: string) => localStorage.setItem(NAME_KEY, n.trim());

export function openRoom(r: { id: string; name: string }) {
  openTab({ type: "room", roomId: r.id }, r.name, `room-${r.id}`);
}

let started = false;
/** Listen to rooms events (once per window). */
export function startRooms() {
  if (started) return;
  started = true;
  const bump = (room: string) => useRooms.setState((s) => ({ ticks: { ...s.ticks, [room]: (s.ticks[room] ?? 0) + 1 } }));
  void listen<{ room: string; what: string }>("rooms-changed", (e) => {
    bump(e.payload.room);
    if (e.payload.what !== "doc" && e.payload.what !== "presence") void useRooms.getState().refresh(false).catch(() => {});
  });
  void listen<JoinRequest>("room-join-request", (e) => useRooms.setState((s) => ({ pending: [...s.pending.filter((p) => p.requestId !== e.payload.requestId), e.payload] })));
  void listen<{ requestId: string }>("room-join-resolved", (e) => useRooms.setState((s) => ({ pending: s.pending.filter((p) => p.requestId !== e.payload.requestId) })));
  void listen<{ room: string; running: boolean; error?: string | null }>("room-agent", (e) => {
    useRooms.setState((s) => ({ agents: { ...s.agents, [e.payload.room]: e.payload.running }, agentErrors: { ...s.agentErrors, [e.payload.room]: e.payload.error ?? undefined } }));
    bump(e.payload.room);
  });
}
