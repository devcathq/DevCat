import { useMemo } from "react";
import { create } from "zustand";
import { api } from "../lib/api";
import type { ConnectionConfig, Project } from "../lib/types";
import { broadcast, launchFocus, onBroadcast, windowStorage } from "../lib/windows";

interface ConnectionsState {
  connections: ConnectionConfig[];
  projects: Project[];
  /** ids of connections with an open backend session */
  open: Set<string>;
  load: () => Promise<void>;
  save: (c: ConnectionConfig) => Promise<ConnectionConfig>;
  remove: (id: string) => Promise<void>;
  markOpen: (id: string, open: boolean) => void;
  /** markOpen for this window only (per-window sessions such as a Kubernetes tab's). */
  markOpenLocal: (id: string, open: boolean) => void;
  byId: (id: string) => ConnectionConfig | undefined;
  /** Create (no id) or update a project. */
  saveProject: (p: Pick<Project, "name"> & Partial<Project>) => Promise<Project>;
  /** Delete a project; its connections are moved out, or deleted too. */
  removeProject: (id: string, deleteConnections?: boolean) => Promise<void>;
  /** Move connections into a project (null = no project). */
  moveToProject: (ids: string[], projectId: string | null) => Promise<void>;
  reorderProjects: (ids: string[]) => Promise<void>;
  /** Focus mode: only this project's connections are shown (null = all projects). */
  focusProjectId: string | null;
  setFocus: (projectId: string | null) => void;
}

// Per window: the main window remembers it across restarts, other windows only for their lifetime.
const FOCUS_KEY = "devcat.focusProject";
if (launchFocus) windowStorage.setItem(FOCUS_KEY, launchFocus);

/** Reload after a change other windows must see too. */
async function reloadAndNotify(load: () => Promise<void>) {
  await load();
  broadcast({ kind: "connections" });
}

export const useConnections = create<ConnectionsState>((set, get) => ({
  connections: [],
  projects: [],
  open: new Set(),
  focusProjectId: windowStorage.getItem(FOCUS_KEY),
  setFocus: (focusProjectId) => {
    if (focusProjectId) windowStorage.setItem(FOCUS_KEY, focusProjectId); else windowStorage.removeItem(FOCUS_KEY);
    set({ focusProjectId });
  },
  load: async () => {
    const [connections, projects] = await Promise.all([api.listConnections(), api.listProjects()]);
    set({ connections, projects });
    // The focused project was deleted: leave focus mode.
    const f = get().focusProjectId;
    if (f && !projects.some((p) => p.id === f)) get().setFocus(null);
  },
  save: async (c) => {
    const saved = await api.saveConnection(c);
    await reloadAndNotify(get().load);
    get().markOpen(saved.id, false);
    return saved;
  },
  remove: async (id) => {
    await api.deleteConnection(id);
    get().markOpen(id, false);
    await reloadAndNotify(get().load);
  },
  markOpen: (id, isOpen) => {
    setOpen(id, isOpen);
    broadcast({ kind: "conn-open", id, open: isOpen });
  },
  markOpenLocal: (id, isOpen) => setOpen(id, isOpen),
  byId: (id) => get().connections.find((c) => c.id === id),
  saveProject: async (p) => {
    const saved = await api.saveProject({ id: "", color: null, description: null, order: 0, folders: [], mcpWrites: false, ...p });
    await reloadAndNotify(get().load);
    return saved;
  },
  removeProject: async (id, deleteConnections = false) => {
    const members = get().connections.filter((c) => c.projectId === id).map((c) => c.id);
    await api.deleteProject(id, deleteConnections);
    if (deleteConnections) members.forEach((m) => get().markOpen(m, false));
    await reloadAndNotify(get().load);
  },
  moveToProject: async (ids, projectId) => {
    // Optimistic so drag & drop feels instant; reload reconciles.
    set((s) => ({ connections: s.connections.map((c) => (ids.includes(c.id) ? { ...c, projectId } : c)) }));
    try {
      await api.moveConnections(ids, projectId);
    } finally {
      await reloadAndNotify(get().load);
    }
  },
  reorderProjects: async (ids) => {
    set((s) => ({ projects: ids.map((id) => s.projects.find((p) => p.id === id)!).filter(Boolean) }));
    const projects = await api.reorderProjects(ids);
    set({ projects });
    broadcast({ kind: "connections" });
  },
}));

function setOpen(id: string, isOpen: boolean) {
  useConnections.setState((s) => {
    if (s.open.has(id) === isOpen) return s;
    const open = new Set(s.open);
    if (isOpen) open.add(id); else open.delete(id);
    return { open };
  });
}

// Changes made in other windows.
onBroadcast((m) => {
  if (m.kind === "connections") useConnections.getState().load().catch(console.error);
  else if (m.kind === "conn-open") setOpen(m.id, m.open);
});

/** The focused project, if focus mode is on. */
export const useFocusProject = () =>
  useConnections((s) => (s.focusProjectId ? s.projects.find((p) => p.id === s.focusProjectId) ?? null : null));

/** True if a connection is visible under the current focus (always true when not focused). */
export function inFocus(connId: string | null | undefined, s = useConnections.getState()): boolean {
  if (!s.focusProjectId || !connId) return true;
  return s.connections.find((c) => c.id === connId)?.projectId === s.focusProjectId;
}

/** Connections visible under the current focus. Use this for pickers and lists. */
export function useScopedConnections(): ConnectionConfig[] {
  const connections = useConnections((s) => s.connections);
  const focus = useConnections((s) => s.focusProjectId);
  return useMemo(() => (focus ? connections.filter((c) => c.projectId === focus) : connections), [connections, focus]);
}

export function newConnection(kind: ConnectionConfig["kind"], projectId: string | null = null): ConnectionConfig {
  return {
    id: "", name: "", kind, host: "localhost", port: null, user: "", password: "",
    database: null, filePath: null, url: null, sslMode: null, ssh: null,
    color: null, group: null, projectId, options: {}, readOnly: false, mcpAccess: "read",
  };
}
