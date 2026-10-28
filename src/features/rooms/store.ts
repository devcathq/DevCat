// Rooms state for the window: the list, join requests waiting for this DevCat's user, which rooms'
// agents are working, mentions of this user not seen yet, and the events that change them.
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow, UserAttentionType } from "@tauri-apps/api/window";
import { toast } from "sonner";
import { create } from "zustand";
import { IS_MAIN_WINDOW } from "../../lib/windows";
import { openTab, useTabs } from "../../store/tabs";
import { label, rooms, type JoinRequest, type Message, type RoomSummary } from "./api";

interface RoomsState {
  id: string | null;
  list: RoomSummary[];
  pending: JoinRequest[];
  /** Rooms whose agent (this DevCat's) is working. */
  agents: Record<string, boolean>;
  agentErrors: Record<string, string | undefined>;
  dialog: boolean;
  /** What the dialog opens on. */
  dialogMode: "list" | "new" | "join";
  /** Bumped on every change of a room, by room id (tabs refresh on it). */
  ticks: Record<string, number>;
  /** Messages naming this user that they haven't looked at yet, by room id. */
  mentions: Record<string, number>;
  /** The room is on screen: its mentions are seen. */
  seen: (room: string) => void;
  show: (open?: boolean, mode?: "list" | "new" | "join") => void;
  refresh: (start?: boolean) => Promise<void>;
}

export const useRooms = create<RoomsState>((set) => ({
  id: null,
  list: [],
  pending: [],
  agents: {},
  agentErrors: {},
  dialog: false,
  dialogMode: "list",
  ticks: {},
  mentions: {},
  seen: (room) => useRooms.setState((s) => (s.mentions[room] ? { mentions: { ...s.mentions, [room]: 0 } } : s)),
  show: (open = true, mode = "list") => {
    set({ dialog: open, dialogMode: mode });
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
  void listen<{ room: string; roomName: string; message: Message }>("room-mention", (e) => {
    const { room, roomName, message } = e.payload;
    // the room on screen: seen as it arrives
    const active = useTabs.getState().tabs.find((t) => t.id === useTabs.getState().activeId);
    if (active?.type === "room" && active.roomId === room && document.hasFocus()) return;
    useRooms.setState((s) => ({ mentions: { ...s.mentions, [room]: (s.mentions[room] ?? 0) + 1 } }));
    if (!IS_MAIN_WINDOW) return;
    const text = message.text.length > 140 ? `${message.text.slice(0, 140)}…` : message.text;
    toast(`${label(message.author)} in ${roomName}`, { description: text, duration: 15000, action: { label: "Open", onClick: () => openRoom({ id: room, name: roomName }) } });
    if (!document.hasFocus()) void getCurrentWindow().requestUserAttention(UserAttentionType.Informational).catch(() => {});
  });
  void listen<{ room: string; running: boolean; error?: string | null }>("room-agent", (e) => {
    useRooms.setState((s) => ({ agents: { ...s.agents, [e.payload.room]: e.payload.running }, agentErrors: { ...s.agentErrors, [e.payload.room]: e.payload.error ?? undefined } }));
    bump(e.payload.room);
  });
}
