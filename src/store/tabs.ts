import { create } from "zustand";
import type { Tab, TabSpec } from "../tabs/types";

interface TabsState {
  tabs: Tab[];
  activeId: string | null;
  /** The tab `open` last opened or brought forward (`n` changes every time), unlike one that
   *  became active because its neighbour closed. Project focus goes by it. */
  opened: { id: string; n: number } | null;
  /** Open a tab. If `key` matches an existing tab it is focused instead. */
  open: (spec: TabSpec, title: string, key?: string) => string;
  /** Close a tab; a registered close guard (see setCloseGuard) may veto it unless `force`. */
  close: (id: string, force?: boolean) => void;
  closeOthers: (id: string) => void;
  activate: (id: string) => void;
  update: (id: string, patch: Partial<Tab>) => void;
  /** Move a tab in front of `beforeId` (null: to the end). */
  move: (id: string, beforeId: string | null) => void;
}

let seq = 0;

/** Asked before a tab closes (e.g. "discard unsaved edits?"); resolve false to keep it open. */
type CloseGuard = () => boolean | Promise<boolean>;
const closeGuards = new Map<string, CloseGuard>();

/** Register (or clear with null) the close guard of a tab. Returns a cleanup for effects. */
export function setCloseGuard(id: string, guard: CloseGuard | null): () => void {
  if (guard) closeGuards.set(id, guard);
  else closeGuards.delete(id);
  return () => { if (closeGuards.get(id) === guard) closeGuards.delete(id); };
}

async function mayClose(id: string): Promise<boolean> {
  const guard = closeGuards.get(id);
  return !guard || (await guard());
}

export const useTabs = create<TabsState>((set, get) => ({
  tabs: [{ id: "welcome", type: "welcome", title: "Welcome" }],
  activeId: "welcome",
  opened: null,
  open: (spec, title, key) => {
    const id = key ?? `${spec.type}-${++seq}`;
    const opened = { id, n: ++seq };
    if (get().tabs.some((t) => t.id === id)) {
      set({ activeId: id, opened });
      return id;
    }
    set((s) => ({ tabs: [...s.tabs, { ...spec, id, title } as Tab], activeId: id, opened }));
    return id;
  },
  close: (id, force) => {
    if (!force && closeGuards.has(id)) {
      void mayClose(id).then((ok) => ok && get().close(id, true));
      return;
    }
    closeGuards.delete(id);
    set((s) => {
      const idx = s.tabs.findIndex((t) => t.id === id);
      const tabs = s.tabs.filter((t) => t.id !== id);
      let activeId = s.activeId;
      if (activeId === id) activeId = tabs[Math.min(idx, tabs.length - 1)]?.id ?? null;
      return { tabs, activeId };
    });
  },
  closeOthers: async (id) => {
    const others = get().tabs.filter((t) => t.id !== id);
    const doomed = new Set<string>();
    for (const t of others) {
      if (closeGuards.has(t.id)) set({ activeId: t.id }); // show the tab being asked about
      if (await mayClose(t.id)) doomed.add(t.id);
    }
    doomed.forEach((d) => closeGuards.delete(d));
    set((s) => ({ tabs: s.tabs.filter((t) => !doomed.has(t.id)), activeId: id }));
  },
  activate: (id) => set({ activeId: id }),
  update: (id, patch) =>
    set((s) => ({ tabs: s.tabs.map((t) => (t.id === id ? ({ ...t, ...patch } as Tab) : t)) })),
  move: (id, beforeId) =>
    set((s) => {
      const tab = s.tabs.find((t) => t.id === id);
      if (!tab || beforeId === id) return s;
      const rest = s.tabs.filter((t) => t.id !== id);
      const at = beforeId === null ? rest.length : rest.findIndex((t) => t.id === beforeId);
      if (at < 0) return s;
      return { tabs: [...rest.slice(0, at), tab, ...rest.slice(at)] };
    }),
}));

/** Convenience helpers used across features. */
export const openTab = (spec: TabSpec, title: string, key?: string) => useTabs.getState().open(spec, title, key);
