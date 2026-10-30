// "Ask DevCat Agent" on a connection (a database, Redis, MongoDB, a cluster): a DevCat Agent chat
// about it, which knows its tables or namespaces from the start (agent_tools.rs `brief`), so the
// user can ask right away. One tab per connection: asking again shows it.
import { homeDir } from "@tauri-apps/api/path";
import type { ConnectionConfig } from "../../lib/types";
import { useConnections } from "../../store/connections";
import { openTab } from "../../store/tabs";
import { toast } from "sonner";
import { loadProviders, openManageProviders, useProviders } from "../claude/providers";

/** Kinds the agent has tools for (agent_tools.rs). */
export const askableKind = (k: ConnectionConfig["kind"]) => k !== "ssh" && k !== "rabbitmq" && k !== "docker";

export async function openConnectionAgent(conn: ConnectionConfig) {
  if (conn.mcpAccess === "off") {
    toast.error(`AI access is off for ${conn.name}. Turn it on in Edit connection → Advanced.`);
    return;
  }
  await loadProviders();
  const st = useProviders.getState();
  const agents = st.providers.filter((p) => p.kind === "agent");
  const provider = agents.find((p) => p.id === st.selectedId) ?? agents[0];
  if (!provider) {
    // no DevCat Agent connection yet: set one up first
    openManageProviders("", "agent");
    return;
  }
  // the project's code folder, so the agent sees its code too; else the home folder
  const project = useConnections.getState().projects.find((p) => p.id === conn.projectId);
  const cwd = project?.folders?.[0] ?? (await homeDir().catch(() => "/"));
  openTab({ type: "claude", cwd, connId: conn.id, providerId: provider.id, sessionId: null, view: "chat" }, `${conn.name} · Agent`, `agent-conn:${conn.id}`);
}
