// App state: paired computers, the selected one, connection states, live agent status, the
// navigation stack (mirrored into browser history so Android's back button works) and toasts.
import { create } from "zustand";
import { call, errMsg, onConn, onEvent, onPath, phone, type ConnPath, type ConnState, type Machine, type PhoneInfo } from "./rpc";
import type { AgentChatInfo, AgentStatus, PtyInfo, Route, TabRoot } from "./types";

export interface Toast { id: number; text: string; tone: "ok" | "err" | "info" }

interface State {
  info: PhoneInfo | null;
  machines: Machine[];
  current: string | null;
  conn: Record<string, { state: ConnState; error?: string | null; path?: ConnPath | null }>;
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

/** The `agents` call in flight: screens asking at the same time share it instead of queuing
 *  more on the connection. */
let agentsCall: Promise<void> | null = null;
export function refreshAgents(): Promise<void> {
  const id = get().current;
  if (!id) return Promise.resolve();
  agentsCall ??= (async () => {
    try {
      const r = await call<{ terminals: PtyInfo[]; agents: AgentStatus[]; chats?: AgentChatInfo[] }>(id, "agents");
      if (get().current !== id) return;
      set({ terms: r.terminals, agents: Object.fromEntries(r.agents.map((a) => [a.ptyId, a])), chats: r.chats ?? [] });
    } catch { /* offline: the connection state shows it */ } finally {
      agentsCall = null;
    }
  })();
  return agentsCall;
}

/** How many agents wait for the user on the selected computer. */
export function waitingCount(s: State = get()): number {
  return Object.values(s.agents).filter((a) => a.status === "waiting" && s.terms.some((t) => t.id === a.ptyId)).length + s.chats.filter((c) => c.waiting).length;
}

// ------------------------------------------------------------------ events
// While the page is on screen: toasts. Off screen the app's Rust side notifies (src-tauri
// watch.rs), also for the other computers and while Android has paused this page.

const folder = (p?: string | null) => (p ?? "").split(/[\\/]/).filter(Boolean).pop() ?? "agent";

export function startStateEvents() {
  onPath((id, path) => {
    // Arrives first while connecting, then on every change.
    const c = get().conn[id] ?? { state: "connecting" as ConnState };
    if (c.state !== "offline") set({ conn: { ...get().conn, [id]: { ...c, path } } });
  });
  onConn((id, state, error, unpaired) => {
    set({ conn: { ...get().conn, [id]: { state, error, path: state === "offline" ? null : get().conn[id]?.path } } });
    if (state === "online" && id === get().current) void refreshAgents();
    if (state === "online") void loadMachines();
    // Browser: the computer no longer knows this pairing, which is now forgotten. Back to pairing.
    if (unpaired) {
      void loadMachines().then((rest) => {
        const top = get().stack[get().stack.length - 1]?.s;
        if (top === "weblanding" || top === "pairing") return; // already pairing with a new code
        toast(error ?? "This browser isn't paired with the computer any more. Scan the QR code again.", "err");
        nav.reset(rest.length ? [{ s: "computers" }] : [{ s: "welcome" }]);
      });
    }
  });
  onEvent<AgentStatus>("agent-status", (a, mid) => {
    if (mid !== get().current) return;
    const prev = get().agents[a.ptyId];
    set({ agents: { ...get().agents, [a.ptyId]: a } });
    if (!get().terms.some((t) => t.id === a.ptyId)) void refreshAgents();
    const changed = prev?.status !== a.status;
    if (!changed) return;
    const watching = (() => { const r = top(); return r.s === "terminal" && r.id === a.ptyId; })();
    if (document.hidden || watching) return;
    if (a.status === "waiting") toast(`${folder(a.cwd)}: ${a.message ?? "needs you"}`, "info");
    else if (a.status === "idle" && prev?.status === "working") toast(`${folder(a.cwd)} finished`, "ok");
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
    if (watching || document.hidden) return;
    const name = folder(c.cwd);
    if (c.waiting && c.waiting !== prev?.waiting) toast(`${name}: ${c.waiting}`, "info");
    else if (!c.busy && prev?.busy) toast(`${name} finished`, "ok");
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
