// App state: paired computers, the selected one, connection states, live agent status, the
// navigation stack (mirrored into browser history so Android's back button works) and toasts.
import { create } from "zustand";
import { call, errMsg, onConn, onEvent, phone, type ConnState, type Machine, type PhoneInfo } from "./rpc";
import type { AgentChatInfo, AgentStatus, PtyInfo, Route, TabRoot } from "./types";

export interface Toast { id: number; text: string; tone: "ok" | "err" | "info" }

interface State {
  info: PhoneInfo | null;
  machines: Machine[];
  current: string | null;
  conn: Record<string, { state: ConnState; error?: string | null }>;
  /** Live terminals and agent status of the current computer. */
  terms: PtyInfo[];
  agents: Record<string, AgentStatus>;
  /** DevCat Agent chats open on the current computer. */
  chats: AgentChatInfo[];
  stack: Route[];
  toasts: Toast[];
}

const LAST = "devcat.mobile.current";

export const useApp = create<State>(() => ({
  info: null,
  machines: [],
  current: localStorage.getItem(LAST),
  conn: {},
  terms: [],
  agents: {},
  chats: [],
  stack: [{ s: "loading" }],
  toasts: [],
}));

const get = useApp.getState;
const set = useApp.setState;

// ------------------------------------------------------------------ navigation

let ignorePops = 0;
window.addEventListener("popstate", () => {
  if (ignorePops > 0) { ignorePops--; return; }
  const st = get().stack;
  if (st.length > 1) set({ stack: st.slice(0, -1) });
});

export const nav = {
  push(r: Route) {
    history.pushState(null, "");
    set({ stack: [...get().stack, r] });
  },
  /** Back one screen (same as the system back button). */
  back() {
    if (get().stack.length > 1) history.back();
  },
  replace(r: Route) {
    const st = get().stack;
    set({ stack: [...st.slice(0, -1), r] });
  },
  /** Start over with this stack (history follows). */
  reset(stack: Route[]) {
    const diff = stack.length - get().stack.length;
    if (diff > 0) for (let i = 0; i < diff; i++) history.pushState(null, "");
    else if (diff < 0) { ignorePops++; history.go(diff); }
    set({ stack });
  },
  /** Switch the bottom tab: Computers › tab root. */
  tab(t: TabRoot) {
    nav.reset([{ s: "computers" }, { s: t }]);
  },
};

export const top = (): Route => { const s = get().stack; return s[s.length - 1]; };

// ------------------------------------------------------------------ toasts

let toastId = 0;
export function toast(text: string, tone: Toast["tone"] = "info") {
  const id = ++toastId;
  set({ toasts: [...get().toasts, { id, text, tone }] });
  setTimeout(() => set({ toasts: get().toasts.filter((t) => t.id !== id) }), tone === "err" ? 5000 : 2800);
}
export const toastErr = (e: unknown) => toast(errMsg(e), "err");

// ------------------------------------------------------------------ computers

export async function loadMachines(): Promise<Machine[]> {
  const machines = await phone.machines();
  set({ machines });
  return machines;
}

export function selectMachine(id: string) {
  localStorage.setItem(LAST, id);
  set({ current: id, terms: [], agents: {}, chats: [] });
  void refreshAgents();
}

export function currentMachine(): Machine | null {
  const { machines, current } = get();
  return machines.find((m) => m.id === current) ?? null;
}

/** Call a command on the selected computer. */
export function host<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  const id = get().current;
  if (!id) return Promise.reject("No computer selected");
  return call<T>(id, cmd, args);
}

export async function refreshAgents() {
  const id = get().current;
  if (!id) return;
  try {
    const r = await call<{ terminals: PtyInfo[]; agents: AgentStatus[]; chats?: AgentChatInfo[] }>(id, "agents");
    if (get().current !== id) return;
    set({ terms: r.terminals, agents: Object.fromEntries(r.agents.map((a) => [a.ptyId, a])), chats: r.chats ?? [] });
  } catch { /* offline: the connection state shows it */ }
}

/** How many agents wait for the user on the selected computer. */
export function waitingCount(s: State = get()): number {
  return Object.values(s.agents).filter((a) => a.status === "waiting" && s.terms.some((t) => t.id === a.ptyId)).length + s.chats.filter((c) => c.waiting).length;
}

// ------------------------------------------------------------------ events

let notify: ((title: string, body: string) => void) | null = null;
export function setLocalNotifier(fn: (title: string, body: string) => void) { notify = fn; }

const folder = (p?: string | null) => (p ?? "").split(/[\\/]/).filter(Boolean).pop() ?? "agent";

export function startStateEvents() {
  onConn((id, state, error) => {
    set({ conn: { ...get().conn, [id]: { state, error } } });
    if (state === "online" && id === get().current) void refreshAgents();
    if (state === "online") void loadMachines();
  });
  onEvent<AgentStatus>("agent-status", (a, mid) => {
    if (mid !== get().current) return;
    const prev = get().agents[a.ptyId];
    set({ agents: { ...get().agents, [a.ptyId]: a } });
    if (!get().terms.some((t) => t.id === a.ptyId)) void refreshAgents();
    const changed = prev?.status !== a.status;
    if (!changed) return;
    const watching = (() => { const r = top(); return r.s === "terminal" && r.id === a.ptyId; })();
    if (a.status === "waiting") {
      const msg = `${folder(a.cwd)}: ${a.message ?? "needs you"}`;
      if (document.hidden) notify?.(`${folder(a.cwd)} needs you`, a.message ?? "");
      else if (!watching) toast(msg, "info");
    } else if (a.status === "idle" && prev?.status === "working") {
      if (document.hidden) notify?.(`${folder(a.cwd)} finished`, "The agent is done.");
      else if (!watching) toast(`${folder(a.cwd)} finished`, "ok");
    }
  });
  onEvent<{ ptyId: string }>("agent-gone", (p, mid) => {
    if (mid !== get().current) return;
    const { [p.ptyId]: _gone, ...rest } = get().agents;
    set({ agents: rest });
  });
  // DevCat Agent chats: started working, waiting for the user, finished
  onEvent<AgentChatInfo>("agent-chat", (c, mid) => {
    if (mid !== get().current) return;
    const prev = get().chats.find((x) => x.sessionId === c.sessionId);
    set({ chats: [c, ...get().chats.filter((x) => x.sessionId !== c.sessionId)] });
    const watching = (() => { const r = top(); return r.s === "agentchat" && r.sessionId === c.sessionId; })();
    if (watching) return;
    const name = folder(c.cwd);
    if (c.waiting && c.waiting !== prev?.waiting) {
      if (document.hidden) notify?.(`${name} needs you`, c.waiting);
      else toast(`${name}: ${c.waiting}`, "info");
    } else if (!c.busy && prev?.busy) {
      if (document.hidden) notify?.(`${name} finished`, c.title || "The agent is done.");
      else toast(`${name} finished`, "ok");
    }
  });
  onEvent<{ sessionId: string }>("agent-chat-gone", (p, mid) => {
    if (mid !== get().current) return;
    set({ chats: get().chats.filter((c) => c.sessionId !== p.sessionId) });
  });
  onEvent<{ id: string }>("pty-exit", (p, mid) => {
    if (mid !== get().current) return;
    set({ terms: get().terms.filter((t) => t.id !== p.id) });
  });
  onEvent<PtyInfo>("remote-pty-opened", (p, mid) => {
    if (mid !== get().current) return;
    if (!get().terms.some((t) => t.id === p.id)) set({ terms: [...get().terms, p] });
  });
}
