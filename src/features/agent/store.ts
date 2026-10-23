// DevCat Agent chats: one per AI workspace tab. The backend runs the turn and reports through
// `agent-event`; this store turns the session log (on open) and the live events into a list of
// items the chat view draws: user messages, answers, tool steps and turn summaries.
import { listen } from "@tauri-apps/api/event";
import { create } from "zustand";
import { api, errMsg } from "../../lib/api";
import { useProviders } from "../claude/providers";
import { chatTabTitle } from "../claude/open";
import { useTabs } from "../../store/tabs";
import type { AiProvider, AgentModel, AgentDecision, AgentLiveEvent, AgentPolicy } from "../../lib/types";
import { ZERO, applyAll, contextWindow, itemsFromLog, lastContext, type ChatView } from "./model";

export { contextWindow, type Item, type ToolStatus } from "./model";

export interface Chat extends ChatView {
  sessionId: string | null;
  /** The DevCat Agent connection (for its model list). */
  providerId: string;
  model: string;
  providerName: string;
  opening: boolean;
  policy: AgentPolicy;
  /** Files attached to the message being written (paths). */
  attachments: string[];
}

const KEY_POLICY = "devcat.agent.policy";

function savedPolicy(): AgentPolicy {
  try {
    const p = localStorage.getItem(KEY_POLICY);
    return p === "auto-edit" || p === "bypass" ? p : "ask";
  } catch {
    return "ask";
  }
}

const emptyChat = (): Chat => ({ sessionId: null, providerId: "", title: "", model: "", providerName: "", busy: false, opening: true, error: null, items: [], usage: ZERO, policy: savedPolicy(), context: null, attachments: [], contextTokens: 0 });

export const useAgent = create<{ chats: Record<string, Chat> }>(() => ({ chats: {} }));

function patch(tabId: string, f: (c: Chat) => Partial<Chat>) {
  useAgent.setState((s) => {
    const c = s.chats[tabId] ?? emptyChat();
    return { chats: { ...s.chats, [tabId]: { ...c, ...f(c) } } };
  });
}

const tabOfSession = (sessionId: string) => Object.entries(useAgent.getState().chats).find(([, c]) => c.sessionId === sessionId)?.[0];

// Streamed text arrives in many small events: apply them once per frame, all at once, so the
// chat redraws at most ~60 times a second however fast the model writes.
const queued = new Map<string, AgentLiveEvent[]>();
let frame = 0;
function flush() {
  frame = 0;
  const batch = [...queued.entries()];
  queued.clear();
  for (const [tabId, events] of batch) patch(tabId, (c) => applyAll(c, events));
}

// Events are numbered (`seq`) per chat: ones that arrive while a chat opens wait for it, and ones
// it already got from `agentOpen` (or before) are skipped.
type Numbered = { event: AgentLiveEvent; seq?: number };
const lastSeq = new Map<string, number>();
const opening = new Map<string, Numbered[]>();

function queue(tabId: string, sessionId: string, events: Numbered[]) {
  const list = queued.get(tabId) ?? [];
  for (const e of events) {
    if (e.seq !== undefined) {
      if (e.seq <= (lastSeq.get(sessionId) ?? 0)) continue;
      lastSeq.set(sessionId, e.seq);
    }
    list.push(e.event);
  }
  queued.set(tabId, list);
  frame ||= requestAnimationFrame(flush);
}

let listening: Promise<unknown> | null = null;
function ensureListening() {
  listening ??= listen<{ sessionId: string; event: AgentLiveEvent; seq?: number }>("agent-event", (ev) => {
    const { sessionId, event, seq } = ev.payload;
    const early = opening.get(sessionId);
    if (early) return void early.push({ event, seq });
    const tabId = tabOfSession(sessionId);
    if (tabId) queue(tabId, sessionId, [{ event, seq }]);
  });
  return listening;
}

/** Open the chat of a tab: resume `sessionId` (running or saved) or start a new session. */
export async function openChat(tabId: string, cwd: string, providerId: string, sessionId: string | null): Promise<string | null> {
  await ensureListening();
  patch(tabId, () => ({ opening: true, error: null }));
  const early: Numbered[] = [];
  if (sessionId) opening.set(sessionId, early);
  try {
    const o = await api.agentOpen(cwd, providerId, sessionId);
    lastSeq.set(o.sessionId, o.seq ?? 0);
    // A turn still running: its events so far come after the log.
    patch(tabId, (c) => applyAll({
      ...c, sessionId: o.sessionId, providerId, title: o.title, model: o.model, providerName: o.providerName, busy: o.busy, opening: false,
      items: itemsFromLog(o.events), usage: o.usage, context: o.context, contextTokens: lastContext(o.events),
    }, o.live ?? []));
    // then what was sent while it opened (minus what `live` already had)
    if (early.length) queue(tabId, o.sessionId, early);
    return o.sessionId;
  } catch (e) {
    patch(tabId, () => ({ opening: false, error: errMsg(e) }));
    return null;
  } finally {
    if (sessionId && opening.get(sessionId) === early) opening.delete(sessionId);
  }
}

export async function closeChat(tabId: string) {
  // Forget the chat right away, so a chat opened again in the same tab (a remount) is never
  // wiped out by this close finishing later.
  const c = useAgent.getState().chats[tabId];
  useAgent.setState((s) => {
    const { [tabId]: _, ...rest } = s.chats;
    return { chats: rest };
  });
  if (c?.sessionId) {
    lastSeq.delete(c.sessionId);
    await api.agentClose(c.sessionId).catch(() => {});
  }
}

export async function send(tabId: string, text: string): Promise<boolean> {
  const c = useAgent.getState().chats[tabId];
  if (!c?.sessionId || c.busy || (!text.trim() && !c.attachments.length)) return false;
  try {
    // the window decides when the conversation gets summarized
    const provider = useProviders.getState().providers.find((p) => p.id === c.providerId);
    const models = provider ? await connectionModels(provider) : [];
    await api.agentSend(c.sessionId, text.trim(), c.policy, c.model || null, c.attachments, contextWindow(c.model, models).tokens);
    patch(tabId, () => ({ attachments: [] }));
    return true;
  } catch (e) {
    patch(tabId, () => ({ error: errMsg(e) }));
    return false;
  }
}

export function stop(tabId: string) {
  const c = useAgent.getState().chats[tabId];
  if (c?.sessionId) void api.agentStop(c.sessionId).catch(() => {});
}

export async function answer(tabId: string, toolUseId: string, decision: AgentDecision) {
  const c = useAgent.getState().chats[tabId];
  if (!c?.sessionId) return;
  // show it at once; the backend confirms with toolCall / toolResult
  patch(tabId, (x) => ({ items: x.items.map((i) => (i.kind === "tool" && i.id === toolUseId ? { ...i, request: null, status: decision.decision === "deny" ? "denied" : "running" } : i)) }));
  await api.agentApprove(c.sessionId, toolUseId, decision).catch((e) => patch(tabId, () => ({ error: errMsg(e) })));
}

export function setPolicy(tabId: string, policy: AgentPolicy) {
  try { localStorage.setItem(KEY_POLICY, policy); } catch { /* private mode */ }
  patch(tabId, () => ({ policy }));
}

/** Answer the model's question (ask_user). */
export async function answerQuestion(tabId: string, toolUseId: string, text: string) {
  const c = useAgent.getState().chats[tabId];
  if (!c?.sessionId || !text.trim()) return;
  patch(tabId, (x) => ({ items: x.items.map((i) => (i.kind === "tool" && i.id === toolUseId ? { ...i, status: "running", question: null } : i)) }));
  await api.agentAnswer(c.sessionId, toolUseId, text.trim()).catch((e) => patch(tabId, () => ({ error: errMsg(e) })));
}

/** Attach files to the message being written: paths, or files dropped / pasted (saved first). */
export async function attach(tabId: string, files: (string | File)[]) {
  const c = useAgent.getState().chats[tabId];
  if (!c?.sessionId) return;
  const paths: string[] = [];
  for (const f of files) {
    if (typeof f === "string") paths.push(f);
    else {
      try {
        paths.push(await api.agentAttach(c.sessionId, f.name || "pasted", await toBase64(f)));
      } catch (e) {
        patch(tabId, () => ({ error: `${f.name}: ${errMsg(e)}` }));
      }
    }
  }
  patch(tabId, (x) => ({ attachments: [...x.attachments, ...paths.filter((p) => !x.attachments.includes(p))] }));
}

export function unattach(tabId: string, path: string) {
  patch(tabId, (x) => ({ attachments: x.attachments.filter((p) => p !== path) }));
}

function toBase64(f: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(String(r.result).replace(/^data:[^,]*,/, ""));
    r.onerror = () => reject(r.error);
    r.readAsDataURL(f);
  });
}

/** Model for this chat's next messages. */
export function setModel(tabId: string, model: string) {
  patch(tabId, () => ({ model }));
}

const modelLists = new Map<string, Promise<AgentModel[]>>();
/** Models of a DevCat Agent connection, with their context windows (asked once per window and connection). */
export function connectionModels(provider: AiProvider): Promise<AgentModel[]> {
  if (!provider.agent) return Promise.resolve([]);
  let p = modelLists.get(provider.id);
  if (!p) {
    p = api.agentModels(provider.agent, provider.id, null).catch(() => []);
    modelLists.set(provider.id, p);
  }
  return p;
}

export function dismissError(tabId: string) {
  patch(tabId, () => ({ error: null }));
}

/** Allow the hooks of a chat's folder as the chat shows them (`digest`); the chat's context updates
 *  when they're on, or with the hooks as they are now when they changed meanwhile. */
export async function trustHooks(tabId: string, digest: string) {
  const c = useAgent.getState().chats[tabId];
  if (!c?.sessionId) return;
  try {
    await api.agentTrustHooks(c.sessionId, digest);
  } catch (e) {
    patch(tabId, () => ({ error: errMsg(e) }));
  }
}

/** Rename a saved chat; its open tab and chat take the new name. */
export async function renameChat(sessionId: string, title: string) {
  await api.agentRename(sessionId, title);
  const name = title.trim().split(/\s+/).join(" ");
  const chats = useAgent.getState().chats;
  const tabs = useTabs.getState();
  for (const t of tabs.tabs) {
    if (t.type !== "claude" || (t.sessionId !== sessionId && chats[t.id]?.sessionId !== sessionId)) continue;
    tabs.update(t.id, { title: chatTabTitle(t.cwd, name) } as never);
    if (chats[t.id]) patch(t.id, () => ({ title: name }));
  }
}
