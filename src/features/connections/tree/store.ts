// Connection tree state: lazily loaded children per (connection, path), expansion,
// selection and collapsed projects.
import { listen } from "@tauri-apps/api/event";
import { create } from "zustand";
import { api, errMsg } from "../../../lib/api";
import type { TreeNode } from "../../../lib/types";
import { useConnections } from "../../../store/connections";
import { nodeKey, SEP } from "./keys";

export { nodeKey, SEP };

export interface NodeState {
  children?: TreeNode[];
  loading?: boolean;
  error?: string | null;
  expanded?: boolean;
  /** bumped on refresh so stale responses are dropped */
  gen?: number;
}

interface TreeState {
  nodes: Record<string, NodeState>;
  /** connections currently running open_connection */
  connecting: Record<string, boolean>;
  /** collapsed projects, keyed by project id */
  collapsedProjects: Record<string, boolean>;
  /** selected row id (see rows.ts) */
  selected: string | null;
  setSelected: (id: string | null) => void;
  toggleProject: (id: string) => void;
  setProjectCollapsed: (id: string, collapsed: boolean) => void;
  load: (connId: string, path: string[]) => Promise<void>;
  setExpanded: (connId: string, path: string[], expanded: boolean) => void;
  /** Expand a database connection, opening the backend session first if needed. */
  expandConnection: (connId: string) => Promise<void>;
  /** Drop cached children of a node and its descendants, then reload it. */
  refresh: (connId: string, path: string[]) => void;
  /** Forget everything cached for a connection (disconnect / delete / edit). */
  clearConn: (connId: string) => void;
  collapseAll: () => void;
  /** Bookmarks section collapsed */
  bookmarksCollapsed: boolean;
  toggleBookmarks: () => void;
  /** "Show only bookmarked" filter */
  bookmarksOnly: boolean;
  setBookmarksOnly: (on: boolean) => void;
  /** Row to scroll into view once it's rendered (set by `reveal`, cleared by the tree). */
  revealId: string | null;
  /** Select a row and scroll it into view. */
  reveal: (rowId: string) => void;
}

const PROJECTS_KEY = "devcat.tree.collapsedProjects";
const loadCollapsed = (): Record<string, boolean> => {
  try { return JSON.parse(localStorage.getItem(PROJECTS_KEY) ?? "{}"); } catch { return {}; }
};

const BOOKMARKS_COLLAPSED_KEY = "devcat.tree.bookmarksCollapsed";
const BOOKMARKS_ONLY_KEY = "devcat.tree.bookmarksOnly";
const loadFlag = (key: string) => localStorage.getItem(key) === "1";
const saveFlag = (key: string, on: boolean) => (on ? localStorage.setItem(key, "1") : localStorage.removeItem(key));

const isUnder = (k: string, key: string, path: string[]) => k === key || k.startsWith(path.length ? key + SEP : key);

export const useTree = create<TreeState>((set, get) => {
  const patch = (key: string, p: Partial<NodeState>) =>
    set((s) => ({ nodes: { ...s.nodes, [key]: { ...s.nodes[key], ...p } } }));

  return {
    nodes: {},
    connecting: {},
    collapsedProjects: loadCollapsed(),
    selected: null,
    bookmarksCollapsed: loadFlag(BOOKMARKS_COLLAPSED_KEY),
    toggleBookmarks: () => {
      const bookmarksCollapsed = !get().bookmarksCollapsed;
      saveFlag(BOOKMARKS_COLLAPSED_KEY, bookmarksCollapsed);
      set({ bookmarksCollapsed });
    },
    bookmarksOnly: loadFlag(BOOKMARKS_ONLY_KEY),
    setBookmarksOnly: (bookmarksOnly) => {
      saveFlag(BOOKMARKS_ONLY_KEY, bookmarksOnly);
      set({ bookmarksOnly });
    },
    revealId: null,
    reveal: (rowId) => set({ selected: rowId, revealId: rowId }),
    setSelected: (selected) => set({ selected }),
    toggleProject: (id) => get().setProjectCollapsed(id, !get().collapsedProjects[id]),
    setProjectCollapsed: (id, collapsed) =>
      set((s) => {
        const collapsedProjects = { ...s.collapsedProjects, [id]: collapsed };
        if (!collapsed) delete collapsedProjects[id];
        localStorage.setItem(PROJECTS_KEY, JSON.stringify(collapsedProjects));
        return { collapsedProjects };
      }),

    load: async (connId, path) => {
      const key = nodeKey(connId, path);
      const cur = get().nodes[key];
      if (cur?.loading) return;
      const gen = (cur?.gen ?? 0) + 1;
      patch(key, { loading: true, error: null, gen });
      try {
        const children = await api.children(connId, path);
        if (get().nodes[key]?.gen !== gen) return;
        patch(key, { children, loading: false, error: null });
      } catch (e) {
        if (get().nodes[key]?.gen !== gen) return;
        patch(key, { loading: false, error: errMsg(e) });
      }
    },

    setExpanded: (connId, path, expanded) => {
      const key = nodeKey(connId, path);
      patch(key, { expanded });
      const n = get().nodes[key];
      if (expanded && !n.children && !n.loading) void get().load(connId, path);
    },

    expandConnection: async (connId) => {
      if (get().connecting[connId]) return;
      const key = nodeKey(connId, []);
      const conns = useConnections.getState();
      if (!conns.open.has(connId)) {
        set((s) => ({ connecting: { ...s.connecting, [connId]: true } }));
        patch(key, { error: null });
        try {
          await api.openConnection(connId);
          conns.markOpen(connId, true);
        } catch (e) {
          patch(key, { expanded: true, error: errMsg(e), children: undefined });
          return;
        } finally {
          set((s) => ({ connecting: { ...s.connecting, [connId]: false } }));
        }
      }
      patch(key, { expanded: true });
      const n = get().nodes[key];
      if (!n.children || n.error) void get().load(connId, []);
    },

    refresh: (connId, path) => {
      const key = nodeKey(connId, path);
      set((s) => {
        const nodes: Record<string, NodeState> = {};
        for (const [k, v] of Object.entries(s.nodes)) {
          nodes[k] = isUnder(k, key, path)
            ? { expanded: v.expanded, gen: (v.gen ?? 0) + 1 }
            : v;
        }
        return { nodes };
      });
      if (path.length === 0 && !useConnections.getState().open.has(connId)) {
        void get().expandConnection(connId);
      } else {
        void get().load(connId, path);
      }
    },

    clearConn: (connId) =>
      set((s) => {
        const prefix = `${connId}|`;
        const nodes: Record<string, NodeState> = {};
        for (const [k, v] of Object.entries(s.nodes)) if (!k.startsWith(prefix)) nodes[k] = v;
        return { nodes };
      }),

    collapseAll: () =>
      set((s) => {
        const nodes: Record<string, NodeState> = {};
        for (const [k, v] of Object.entries(s.nodes)) nodes[k] = { ...v, expanded: false };
        return { nodes };
      }),
  };
});

// The DevCat Agent dropped a database (agent_tools.rs `drop_database`): show the connection as it is now.
void listen<{ id: string }>("connection-changed", (e) => useTree.getState().refresh(e.payload.id, [])).catch(() => {});
