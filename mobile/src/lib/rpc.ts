// Talking to the phone's Rust side (mobile/src-tauri): paired computers, and calls that run on
// a computer. Events from computers arrive as "m-event" and are dispatched here by name.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export interface Machine {
  id: string;
  name: string;
  addrs: string[];
  os: string;
  appVersion: string;
  access: "full" | "readonly" | "";
  addedAt: number;
  lastSeen?: number | null;
  manualAddrs: string[];
  /** The computer's relay (`public` or a URL), when it allows other networks. */
  relay?: string | null;
}

export interface PhoneInfo {
  deviceId: string;
  deviceName: string;
  platform: string;
  version: string;
  online: string[];
}

export type ConnState = "connecting" | "online" | "offline";
/** How the connection travels right now (the app only; browsers always use the relay). */
export interface ConnPath { direct: boolean; rttMs: number; relay?: string | null }

export const phone = {
  info: () => invoke<PhoneInfo>("m_info"),
  setName: (name: string) => invoke<void>("m_set_name", { name }),
  machines: () => invoke<Machine[]>("m_machines"),
  pair: (link: string, addr?: string | null) => invoke<Machine>("m_pair", { link, addr: addr ?? null }),
  connect: (id: string) => invoke<unknown>("m_connect", { id }),
  disconnect: (id: string) => invoke<void>("m_disconnect", { id }),
  forget: (id: string) => invoke<Machine[]>("m_forget", { id }),
  setAddrs: (id: string, addrs: string[]) => invoke<Machine[]>("m_set_addrs", { id, addrs }),
  networkChanged: () => invoke<void>("m_network_changed"),
};

/** Run a command on a computer (see src-tauri/src/remote/dispatch.rs for the list). */
export function call<T>(machineId: string, cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  return invoke<T>("m_call", { id: machineId, cmd, args });
}

export function errMsg(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return JSON.stringify(e);
}

type Handler = (payload: any, machineId: string) => void;
const handlers = new Map<string, Set<Handler>>();
type ConnHandler = (id: string, state: ConnState, error?: string | null, unpaired?: boolean) => void;
const connHandlers = new Set<ConnHandler>();
type PairHandler = (p: { stage: "connecting" | "waiting" | "done"; hostName: string }) => void;
const pairHandlers = new Set<PairHandler>();
type PathHandler = (id: string, path: ConnPath | null) => void;
const pathHandlers = new Set<PathHandler>();

let started: Promise<void> | null = null;
/** Subscribe to the Rust events once. */
export function startEvents(): Promise<void> {
  if (!started) {
    started = Promise.all([
      listen<{ id: string; event: string; payload: unknown }>("m-event", (e) => {
        handlers.get(e.payload.event)?.forEach((h) => h(e.payload.payload, e.payload.id));
      }),
      listen<{ id: string; state: ConnState; error?: string | null; unpaired?: boolean }>("m-conn", (e) => {
        connHandlers.forEach((h) => h(e.payload.id, e.payload.state, e.payload.error, e.payload.unpaired));
      }),
      listen<{ id: string; path: ConnPath | null }>("m-path", (e) => {
        pathHandlers.forEach((h) => h(e.payload.id, e.payload.path));
      }),
      listen<{ stage: "connecting" | "waiting" | "done"; hostName: string }>("m-pair", (e) => {
        pairHandlers.forEach((h) => h(e.payload));
      }),
    ]).then(() => undefined);
  }
  return started;
}

/** Listen to an event from computers (only `machineId`'s when given). Returns unsubscribe. */
export function onEvent<T = any>(event: string, h: (payload: T, machineId: string) => void, machineId?: string): () => void {
  const wrapped: Handler = (p, id) => { if (!machineId || id === machineId) h(p as T, id); };
  const set = handlers.get(event) ?? new Set<Handler>();
  set.add(wrapped);
  handlers.set(event, set);
  return () => { set.delete(wrapped); };
}

export function onConn(h: ConnHandler): () => void {
  connHandlers.add(h);
  return () => { connHandlers.delete(h); };
}

export function onPath(h: PathHandler): () => void {
  pathHandlers.add(h);
  return () => { pathHandlers.delete(h); };
}

export function onPair(h: PairHandler): () => void {
  pairHandlers.add(h);
  return () => { pairHandlers.delete(h); };
}
