// What you sent to DevCat Agent in a folder, for ↑ / ↓ in the chat box (as in Claude Code: the
// history is the project's, across its chats). Kept in localStorage, newest last.
const MAX = 200;

const key = (cwd: string) => `devcat.agent.history.${cwd.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase()}`;

export function loadHistory(cwd: string): string[] {
  try {
    const v = JSON.parse(localStorage.getItem(key(cwd)) ?? "[]");
    return Array.isArray(v) ? v.filter((s): s is string => typeof s === "string") : [];
  } catch {
    return [];
  }
}

/** Add a sent message (not twice in a row). */
export function remember(cwd: string, text: string) {
  const t = text.trim();
  if (!t) return;
  const list = loadHistory(cwd).filter((s, i, a) => !(i === a.length - 1 && s === t));
  list.push(t);
  try {
    localStorage.setItem(key(cwd), JSON.stringify(list.slice(-MAX)));
  } catch {
    /* private mode or full */
  }
}

/** The history for the chat box: the folder's, or else this chat's own messages (a chat from
 *  before the history was kept). */
export function historyFor(cwd: string, chatMessages: string[]): string[] {
  const stored = loadHistory(cwd);
  return stored.length ? stored : chatMessages.filter((m) => m.trim());
}
