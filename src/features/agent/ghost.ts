// Ghost chats: a DevCat Agent chat in a window of its own that starts from nothing and keeps
// nothing. No project: it works in an empty scratch folder of its own, without rule files, skills,
// MCP servers, hooks, DevCat connections, memory, notes or rooms. Its log lives in a temporary
// folder only, and closing the window deletes it all (agent.rs `ghost_dir`). It isn't in Sessions
// or on a phone, and what you type isn't kept for ↑ in the chat box.
import { findProvider, loadProviders, openManageProviders, useProviders } from "../claude/providers";
import { openNewWindow } from "../../lib/windows";

/** A ghost chat, run by `providerId` when it's a DevCat Agent (else the selected or first DevCat
 *  Agent connection; with none, the dialog to add one opens). */
export async function openGhostChat(providerId?: string | null) {
  await loadProviders();
  const st = useProviders.getState();
  const agents = st.providers.filter((p) => p.kind === "agent");
  const asked = findProvider(st.providers, providerId);
  const provider = asked?.kind === "agent" ? asked : agents.find((p) => p.id === st.selectedId) ?? agents[0];
  if (!provider) {
    // ghost chats run on DevCat Agent: set one up first
    openManageProviders("", "agent");
    return false;
  }
  // no folder: the backend gives it an empty scratch folder of its own
  return openNewWindow(null, { type: "claude", cwd: "", providerId: provider.id, sessionId: null, view: "chat", ghost: true, title: "Ghost chat" });
}
