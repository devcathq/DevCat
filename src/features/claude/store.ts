// AI workspace state, keyed by tab id: the agent terminal (pty id, launch options, Claude session to
// resume), git status and the last code review. Session / launch-option features are Claude-only.
// Reviews stream over "claude-msg" / "claude-exit"; `initClaude()` subscribes once (idempotent)
// and routes each event to its tab by runId.
import { listen } from "@tauri-apps/api/event";
import { create } from "zustand";
import { api, errMsg } from "../../lib/api";
import { isRemote } from "../../lib/remotePath";
import type { ClaudeInfo, ClaudeMsg, ClaudePermissionMode, GitStatus } from "../../lib/types";
import { useTabs } from "../../store/tabs";

export interface Finding {
  severity: string;
  file?: string;
  line?: number | null;
  title: string;
  body?: string;
  suggestion?: string;
}

export interface ReviewState {
  runId: string | null;
  status: "idle" | "running" | "done" | "error";
  scope: "uncommitted" | "staged" | "branch";
  base: string;
  model: string;
  progress: string[];
  summary?: string;
  findings?: Finding[] | null;
  /** Raw markdown when the result could not be parsed as findings. */
  markdown?: string | null;
  error?: string | null;
  costUsd?: number;
  durationMs?: number;
}

export interface LaunchOptions { model: string; permissionMode: ClaudePermissionMode | "" }

export interface TabState {
  cwd: string;
  /** The tab runs Claude Code (sessions, launch options and reviews apply); false = another AI CLI. */
  isClaude: boolean;
  /** Session the next `claude` launch resumes (null = fresh session). */
  sessionId: string | null;
  /** `sessionId` is the id a new chat was launched with (`--session-id`): no transcript until its first message. */
  sessionFresh: boolean;
  /** Launch options picked in the header vs. the ones the running process was started with. */
  options: LaunchOptions;
  launched: LaunchOptions | null;
  launchedAt: number | null;
  /** Pty of the claude terminal while it runs. */
  ptyId: string | null;
  /** Bump to (re)start the claude process. */
  startSeq: number;
  /** A repository inside the folder to show in Diff (the folder itself isn't one, or the user picked
   *  another); null = the folder. */
  repo: string | null;
  git: GitStatus | null;
  gitError: string | null;
  gitLoading: boolean;
  /** Bumped after every git poll (the diff view re-reads the open file on it). */
  gitTick: number;
  /** Diff view: file to select (from a review finding). */
  diffFocus: { path: string; line?: number | null; seq: number } | null;
  review: ReviewState;
}

const newReview = (): ReviewState => ({ runId: null, status: "idle", scope: "uncommitted", base: "main", model: "", progress: [] });

const newTab = (cwd: string, sessionId: string | null): TabState => ({
  cwd, isClaude: true, sessionId, sessionFresh: false, options: { model: "", permissionMode: "" }, launched: null, launchedAt: null, ptyId: null, startSeq: 0,

  repo: null, git: null, gitError: null, gitLoading: false, gitTick: 0, diffFocus: null, review: newReview(),
});

let seq = 0;

interface ClaudeStore {
  tabs: Record<string, TabState>;
  info: ClaudeInfo | null;
  infoError: string | null;
  infoLoaded: boolean;
}

export const useClaude = create<ClaudeStore>(() => ({ tabs: {}, info: null, infoError: null, infoLoaded: false }));

const get = (tabId: string) => useClaude.getState().tabs[tabId];
function patch(tabId: string, p: Partial<TabState> | ((s: TabState) => Partial<TabState>)) {
  useClaude.setState((st) => {
    const cur = st.tabs[tabId];
    if (!cur) return st;
    const next = typeof p === "function" ? p(cur) : p;
    return { tabs: { ...st.tabs, [tabId]: { ...cur, ...next } } };
  });
}
function patchReview(tabId: string, p: Partial<ReviewState> | ((r: ReviewState) => Partial<ReviewState>)) {
  patch(tabId, (s) => ({ review: { ...s.review, ...(typeof p === "function" ? p(s.review) : p) } }));
}


/** Create the tab's state if missing. */
export function ensureTab(tabId: string, cwd: string, sessionId: string | null | undefined) {
  initClaude();
  if (!get(tabId)) useClaude.setState((st) => ({ tabs: { ...st.tabs, [tabId]: newTab(cwd, sessionId ?? null) } }));
}

export async function loadInfo(force = false) {
  const st = useClaude.getState();
  if (st.infoLoaded && !force) return;
  useClaude.setState({ infoLoaded: true });
  try {
    useClaude.setState({ info: await api.claudeInfo(), infoError: null });
  } catch (e) {
    useClaude.setState({ info: null, infoError: errMsg(e) });
  }
}

// ---------- claude terminal ----------

/** Whether the tab runs Claude Code (set by the tab from its AI connection). */
export function setTabIsClaude(tabId: string, isClaude: boolean) {
  if (get(tabId)?.isClaude !== isClaude) patch(tabId, { isClaude, ...(isClaude ? {} : { sessionId: null, sessionFresh: false }) });
}

/** CLI args for the next launch (Claude: session / model / permission mode; others: none); records what was launched. */
export function claudeLaunchArgs(tabId: string): string[] {
  const s = get(tabId);
  if (!s) return [];
  if (!s.isClaude) {
    patch(tabId, { launched: null, launchedAt: Date.now() });
    return [];
  }
  const args: string[] = [];
  // A new chat's id that detectSession never confirmed (its launch was cancelled): pick a new one.
  let sessionId = s.sessionFresh ? null : s.sessionId;
  let sessionFresh = false;
  if (sessionId) args.push("--resume", sessionId);
  // New chat: we pick its session id, so the tab knows its own session before the first message (on
  // SSH hosts we can't see transcripts, so a restart couldn't tell whether there's one to resume).
  else if (!isRemote(s.cwd)) {
    sessionId = crypto.randomUUID();
    sessionFresh = true;
    args.push("--session-id", sessionId);
  }
  if (s.options.model) args.push("--model", s.options.model);
  // Manual is the CLI's default: omit the flag.
  if (s.options.permissionMode && s.options.permissionMode !== "manual") args.push("--permission-mode", s.options.permissionMode);
  patch(tabId, { sessionId, sessionFresh, launched: { ...s.options }, launchedAt: Date.now() });
  return args;
}

export const setPty = (tabId: string, ptyId: string | null) => patch(tabId, { ptyId });

export const setLaunchOptions = (tabId: string, p: Partial<LaunchOptions>) => patch(tabId, (s) => ({ options: { ...s.options, ...p } }));

/**
 * Session the claude process worked on since it was launched: the newest session of the folder
 * updated after launch (covers /clear and /resume inside the TUI) that no other tab is on, else the
 * one it was started with — or none, for a new chat that never got a message.
 */
export async function detectSession(tabId: string): Promise<string | null> {
  const s = get(tabId);
  if (!s) return null;
  if (!s.isClaude) return null;
  try {
    const list = await api.claudeSessions(s.cwd);
    const since = (s.launchedAt ?? 0) - 2000;
    // Another tab's agent in the same folder keeps its session updated too: never take that one.
    const others = new Set(Object.entries(useClaude.getState().tabs).flatMap(([id, t]) => (id !== tabId && t.sessionId ? [t.sessionId] : [])));
    const newest = list
      .filter((x) => x.updatedAt && Date.parse(x.updatedAt) >= since && !others.has(x.id))
      .sort((a, b) => Date.parse(b.updatedAt!) - Date.parse(a.updatedAt!))[0];
    if (newest) {
      patch(tabId, { sessionId: newest.id, sessionFresh: false });
      return newest.id;
    }
    // New chat with nothing written yet: there's nothing to resume.
    if (s.sessionFresh) {
      patch(tabId, { sessionId: null, sessionFresh: false });
      return null;
    }
  } catch { /* backend not ready: keep the known session */ }
  return s.sessionId;
}

/** Restart the agent (Claude: resuming the current session, e.g. after changing launch options or an exit). */
export async function restartClaude(tabId: string) {
  await detectSession(tabId);
  patch(tabId, (s) => ({ startSeq: s.startSeq + 1 }));
}

/** Restart the agent with a fresh session. */
export function newSession(tabId: string) {
  patch(tabId, (s) => ({ sessionId: null, sessionFresh: false, startSeq: s.startSeq + 1 }));
}

/** Paste text into the claude prompt without submitting it (bracketed paste). */
export async function pasteToClaude(tabId: string, text: string): Promise<boolean> {
  const id = get(tabId)?.ptyId;
  if (!id) return false;
  try {
    await api.ptyWrite(id, `\x1b[200~${text}\x1b[201~`);
    return true;
  } catch {
    return false;
  }
}

/** Type a prompt into the agent and submit it (Enter). False when the agent isn't running. */
export async function sendToAgent(tabId: string, text: string): Promise<boolean> {
  const id = get(tabId)?.ptyId;
  if (!id) return false;
  try {
    await api.ptyWrite(id, text);
    // Separate write so TUIs don't take the Enter as part of a paste.
    await new Promise((r) => setTimeout(r, 60));
    await api.ptyWrite(id, "\r");
    return true;
  } catch {
    return false;
  }
}

// ---------- git ----------

export async function refreshGit(tabId: string) {
  const s = get(tabId);
  if (!s || s.gitLoading) return;
  patch(tabId, { gitLoading: true });
  try {
    const git = await api.gitStatus(s.repo ?? s.cwd);
    // Keep the old object when nothing changed so subscribers don't re-render on every poll.
    patch(tabId, (cur) => ({
      git: JSON.stringify(cur.git) === JSON.stringify(git) ? cur.git : git,
      gitError: null, gitLoading: false, gitTick: cur.gitTick + 1,
    }));
  } catch (e) {
    patch(tabId, { gitError: errMsg(e), gitLoading: false });
  }
}

/** Show the changes of `repo` (a repository inside the tab's folder; null = the folder itself). */
export function setRepo(tabId: string, repo: string | null) {
  patch(tabId, { repo, git: null, gitError: null });
  void refreshGit(tabId);
}

export const focusDiff = (tabId: string, path: string, line?: number | null) =>
  patch(tabId, { diffFocus: { path, line, seq: ++seq } });

// ---------- review (stream-json run) ----------

interface ExitPayload { runId: string; code?: number | null; error?: string | null }

const runs = new Map<string, string>(); // runId → tabId
/** Events for runs not registered yet (claudeReview still resolving). */
const orphan = new Map<string, Array<{ msg?: ClaudeMsg; exit?: ExitPayload }>>();
/** Runs we stopped/abandoned: drop their late events instead of buffering. */
const dead = new Set<string>();
function forget(runId: string) {
  runs.delete(runId);
  dead.add(runId);
}

function register(runId: string, tabId: string) {
  runs.set(runId, tabId);
  const buf = orphan.get(runId);
  orphan.delete(runId);
  buf?.forEach((e) => (e.msg ? route(runId, e.msg) : e.exit && routeExit(e.exit)));
}

function buffer(runId: string, e: { msg?: ClaudeMsg; exit?: ExitPayload }) {
  if (dead.has(runId)) return;
  const b = orphan.get(runId) ?? [];
  b.push(e);
  orphan.set(runId, b.slice(-2000));
}

function route(runId: string, msg: ClaudeMsg) {
  const tabId = runs.get(runId);
  if (!tabId) return buffer(runId, { msg });
  onReviewMsg(tabId, runId, msg);
}

function routeExit(p: ExitPayload) {
  const tabId = runs.get(p.runId);
  if (!tabId) return buffer(p.runId, { exit: p });
  runs.delete(p.runId);
  const s = get(tabId);
  if (!s || s.review.runId !== p.runId) return;
  const failed = !!p.error || (p.code != null && p.code !== 0);
  if (s.review.status === "running")
    patchReview(tabId, { status: "error", error: failed ? p.error || `Claude exited with code ${p.code}` : "Review ended without a result", runId: null });
  else patchReview(tabId, { runId: null });
}

let inited = false;
export function initClaude() {
  if (inited) return;
  inited = true;
  void listen<{ runId: string; msg: ClaudeMsg }>("claude-msg", (e) => route(e.payload.runId, e.payload.msg));
  void listen<ExitPayload>("claude-exit", (e) => routeExit(e.payload));
  // Forget closed tabs and stop their reviews (their terminals close on unmount).
  useTabs.subscribe((t) => {
    const open = new Set(t.tabs.map((x) => x.id));
    const st = useClaude.getState().tabs;
    const gone = Object.keys(st).filter((id) => !open.has(id));
    if (!gone.length) return;
    for (const id of gone) {
      const r = st[id].review.runId;
      if (r) {
        forget(r);
        void api.claudeStop(r).catch(() => {});
      }
    }
    useClaude.setState((cur) => {
      const tabs = { ...cur.tabs };
      gone.forEach((id) => delete tabs[id]);
      return { tabs };
    });
  });
}

export function setReviewOptions(tabId: string, p: Partial<Pick<ReviewState, "scope" | "base" | "model">>) {
  patchReview(tabId, p);
}

export async function startReview(tabId: string) {
  const s = get(tabId);
  if (!s) return;
  if (s.review.runId) {
    forget(s.review.runId);
    void api.claudeStop(s.review.runId).catch(() => {});
  }
  const { scope, base, model } = s.review;
  patchReview(tabId, { status: "running", progress: [], findings: null, summary: undefined, markdown: null, error: null, costUsd: undefined, durationMs: undefined, runId: null });
  try {
    const runId = await api.claudeReview(s.cwd, scope, scope === "branch" ? base || "main" : null, model || null);
    // runId must be set before replaying buffered events.
    patchReview(tabId, { runId });
    register(runId, tabId);
  } catch (e) {
    patchReview(tabId, { status: "error", error: errMsg(e) });
  }
}

export async function cancelReview(tabId: string) {
  const s = get(tabId);
  if (!s?.review.runId) return;
  const id = s.review.runId;
  forget(id);
  patchReview(tabId, { status: "idle", runId: null, error: null });
  try { await api.claudeStop(id); } catch { /* ignore */ }
}

const asObj = (v: unknown): Record<string, unknown> => (v && typeof v === "object" ? (v as Record<string, unknown>) : {});
const asStr = (v: unknown): string | undefined => (typeof v === "string" ? v : undefined);
const asNum = (v: unknown): number | undefined => (typeof v === "number" && isFinite(v) ? v : undefined);

function toolSummary(name: string, input: Record<string, unknown>): string {
  const s = (k: string) => asStr(input[k]) ?? "";
  switch (name) {
    case "Read": case "Write": case "Edit": case "MultiEdit": return `${name} ${s("file_path")}`;
    case "Bash": return `Bash ${s("command").split("\n")[0]}`;
    case "Grep": case "Glob": return `${name} ${s("pattern")}`;
    case "WebFetch": return `WebFetch ${s("url")}`;
    case "WebSearch": return `WebSearch ${s("query")}`;
    case "Task": case "Agent": return `${name} ${s("description")}`;
    default: return name;
  }
}

function parseFindings(v: unknown): { summary?: string; findings: Finding[] } | null {
  const o = asObj(v);
  if (!Array.isArray(o.findings)) return null;
  const findings = (o.findings as unknown[]).map(asObj).map((f) => ({
    severity: String(f.severity ?? "minor").toLowerCase(),
    file: asStr(f.file),
    line: asNum(f.line) ?? (typeof f.line === "string" ? parseInt(f.line, 10) || null : null),
    title: String(f.title ?? "Finding"),
    body: asStr(f.body),
    suggestion: asStr(f.suggestion),
  }));
  return { summary: asStr(o.summary), findings };
}

function tryJson(text: string): unknown {
  const t = text.trim();
  const fenced = /```(?:json)?\s*([\s\S]*?)```/.exec(t);
  for (const c of [t, fenced?.[1], t.slice(t.indexOf("{"), t.lastIndexOf("}") + 1)]) {
    if (!c) continue;
    try { return JSON.parse(c); } catch { /* next */ }
  }
  return null;
}

function onReviewMsg(tabId: string, runId: string, msg: ClaudeMsg) {
  const s = get(tabId);
  if (!s || s.review.runId !== runId) return;
  if (msg.type === "assistant") {
    if (msg.parent_tool_use_id) return;
    const content = asObj(msg.message).content;
    const lines = (Array.isArray(content) ? content : []).map(asObj).flatMap((b) =>
      b.type === "tool_use" ? [toolSummary(String(b.name ?? "tool"), asObj(b.input))]
        : b.type === "text" && asStr(b.text) ? [(b.text as string).split("\n")[0].slice(0, 160)] : []);
    if (lines.length) patchReview(tabId, (r) => ({ progress: [...r.progress, ...lines].slice(-200) }));
  } else if (msg.type === "result") {
    const text = asStr(msg.result) ?? "";
    const parsed = parseFindings(msg.structured_output) ?? parseFindings(tryJson(text));
    patchReview(tabId, {
      status: msg.is_error ? "error" : "done",
      error: msg.is_error ? text || String(msg.subtype ?? "Review failed") : null,
      summary: parsed?.summary,
      findings: parsed?.findings ?? null,
      markdown: parsed ? null : text,
      costUsd: asNum(msg.total_cost_usd),
      durationMs: asNum(msg.duration_ms),
    });
  } else if (msg.type === "control_request") {
    // Reviews are read-only; refuse anything that asks for permission.
    const req = asObj(msg.request);
    if (req.subtype === "can_use_tool")
      void api.claudePermission(runId, String(msg.request_id ?? ""), { behavior: "deny", message: "Code review is read-only." }).catch(() => {});
  }
}
