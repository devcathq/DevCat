import { openTab, useTabs } from "../../store/tabs";
import { CLAUDE_PROVIDER_ID, findProvider, useProviders } from "./providers";

export type ClaudeView = "chat" | "diff" | "review";

export function basename(p: string): string {
  const parts = p.replace(/[\\/]+$/, "").split(/[\\/]/);
  return parts[parts.length - 1] || p;
}

let newChats = 0;

/** Tab key: `claude:${cwd}:${sessionId}` for a resumed session (opening it again focuses that tab).
 *  A new chat always gets a tab of its own, whatever is already open for the folder. */
export function claudeTabKey(cwd: string, sessionId?: string | null): string {
  if (sessionId) return `claude:${cwd}:${sessionId}`;
  return `claude:${cwd}:new-${Date.now().toString(36)}-${++newChats}`;
}

/** A chat tab's title: the folder, then the chat's name cut to fit. */
export function chatTabTitle(cwd: string, chatTitle: string): string {
  const t = chatTitle.trim();
  return `${basename(cwd)} · ${t.slice(0, 28)}${t.length > 28 ? "…" : ""}`;
}

/** Open (or focus) the AI workspace tab for a folder. Returns the tab id.
 *  providerId defaults to the AI connection selected for new chats; sessions are Claude's. */
export function openClaudeWorkspace(
  cwd: string,
  opts?: { sessionId?: string | null; view?: ClaudeView; title?: string | null; providerId?: string | null },
): string {
  const st = useProviders.getState();
  // A session belongs to Claude Code unless it's a DevCat Agent chat (those name their connection).
  const asked = findProvider(st.providers, opts?.providerId ?? st.selectedId);
  const provider = opts?.sessionId && asked?.kind !== "agent"
    ? findProvider(st.providers, CLAUDE_PROVIDER_ID)!
    : asked ?? findProvider(st.providers, CLAUDE_PROVIDER_ID)!;
  const isClaude = provider.id === CLAUDE_PROVIDER_ID;
  const sessionId = isClaude || provider.kind === "agent" ? (opts?.sessionId ?? null) : null;
  const key = claudeTabKey(cwd, sessionId);
  const title = opts?.title?.trim() ? chatTabTitle(cwd, opts.title) : isClaude ? basename(cwd) : `${basename(cwd)} · ${provider.name}`;
  const id = openTab({ type: "claude", cwd, sessionId, view: opts?.view ?? "chat", providerId: isClaude ? null : provider.id }, title, key);
  // Tab may already exist: still honor a requested view.
  if (opts?.view) useTabs.getState().update(id, { view: opts.view } as never);
  return id;
}
