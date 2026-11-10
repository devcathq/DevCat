import { create } from "zustand";

/** "claude" = the AI workspace (internal id kept). */
export type SidebarView = "claude" | "connections" | "rooms" | "network" | "git" | "notes" | "tickets";

const COLLAPSED_KEY = "devcat.sidebar.collapsed";

interface UiState {
  sidebar: SidebarView;
  setSidebar: (v: SidebarView) => void;
  /** Sidebar hidden (the activity bar stays). */
  sidebarCollapsed: boolean;
  setSidebarCollapsed: (v: boolean) => void;
  /** Connection dialog: null = closed; id = edit, else new (optionally preselecting kind / project,
   *  or `parentId`: a new variant of that SSH session). */
  connectionDialog: { id: string | null; kind?: string; projectId?: string | null; parentId?: string | null } | null;
  openConnectionDialog: (id?: string | null, kind?: string, projectId?: string | null, parentId?: string | null) => void;
  closeConnectionDialog: () => void;
  /** Status bar message */
  status: string;
  setStatus: (s: string) => void;
}

export const useUi = create<UiState>((set) => ({
  sidebar: "claude",
  setSidebar: (sidebar) => set({ sidebar }),
  sidebarCollapsed: localStorage.getItem(COLLAPSED_KEY) === "1",
  setSidebarCollapsed: (sidebarCollapsed) => {
    localStorage.setItem(COLLAPSED_KEY, sidebarCollapsed ? "1" : "0");
    set({ sidebarCollapsed });
  },
  connectionDialog: null,
  openConnectionDialog: (id = null, kind, projectId, parentId) => set({ connectionDialog: { id, kind, projectId, parentId } }),
  closeConnectionDialog: () => set({ connectionDialog: null }),
  status: "Ready",
  setStatus: (status) => set({ status }),
}));
