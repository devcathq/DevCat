import { create } from "zustand";
import type { Tab, TabSpec } from "../tabs/types";
import { activateTab, focusPane, initialPanes, moveTab, openBeside, paneTabs, placeTab, removeTab, splitTab, type Panes, type Side } from "./panes";

export type { Side } from "./panes";

interface TabsState extends Panes {
  tabs: Tab[];
  /** The active tab of the focused pane: where the user is. */
  activeId: string | null;
  /** The tab `open` last opened or brought forward (`n` changes every time), unlike one that
   *  became active because its neighbour closed. Project focus goes by it. */
  opened: { id: string; n: number } | null;
  /** Tabs focus mode hides (other projects'): still mounted, not on screen. Set by the workspace. */
  hidden: Record<string, true>;
  /** Open a tab in the focused pane. If `key` matches an existing tab, that one is shown (in its pane) instead. */
  open: (spec: TabSpec, title: string, key?: string) => string;
  /** Open a new tab in a new pane on `side` of `besideId`'s pane. */
  openSplit: (spec: TabSpec, title: string, besideId: string, side: Side) => string;
  /** Close a tab; a registered close guard (see setCloseGuard) may veto it unless `force`. */
  close: (id: string, force?: boolean) => void;
  /** Close the other tabs of `id`'s pane. */
  closeOthers: (id: string) => void;
  /** Show a tab in its pane; unless `focus` is false that pane gets the focus too. */
  activate: (id: string, focus?: boolean) => void;
  update: (id: string, patch: Partial<Tab>) => void;
  /** Move a tab in front of `beforeId` (null: to the end), in the tab order. */
  move: (id: string, beforeId: string | null) => void;
  /** Move a tab into `pane`, in front of `beforeId` (null: after that pane's last tab). */
  moveToPane: (id: string, pane: string, beforeId?: string | null) => void;
  /** Move a tab into a new pane on `side` of `pane` (default: its own). */
  split: (id: string, side: Side, pane?: string) => void;
  focusPane: (pane: string) => void;
  setHidden: (ids: string[]) => void;
}

let seq = 0;
const newPane = () => `pane-${++seq}`;
const panesOf = (s: Panes): Panes => ({ layout: s.layout, paneOf: s.paneOf, paneActive: s.paneActive, focusedPane: s.focusedPane });
/** State for new panes, with `activeId` following them. */
const withPanes = (p: Panes) => ({ ...p, activeId: p.paneActive[p.focusedPane] ?? null });
const idsOf = (tabs: Tab[]) => tabs.map((t) => t.id);

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
  ...withPanes(initialPanes("main", ["welcome"])),
  opened: null,
  hidden: {},
  open: (spec, title, key) => {
    const id = key ?? `${spec.type}-${++seq}`;
    const opened = { id, n: ++seq };
    if (get().tabs.some((t) => t.id === id)) {
      set((s) => ({ ...withPanes(activateTab(panesOf(s), id)), opened }));
      return id;
    }
    set((s) => ({ tabs: [...s.tabs, { ...spec, id, title } as Tab], ...withPanes(placeTab(panesOf(s), id)), opened }));
    return id;
  },
  openSplit: (spec, title, besideId, side) => {
    const id = `${spec.type}-${++seq}`;
    const opened = { id, n: ++seq };
    set((s) => ({ tabs: [...s.tabs, { ...spec, id, title } as Tab], ...withPanes(openBeside(panesOf(s), id, side, newPane(), s.paneOf[besideId])), opened }));
    return id;
  },
  close: (id, force) => {
    if (!force && closeGuards.has(id)) {
      void mayClose(id).then((ok) => ok && get().close(id, true));
      return;
    }
    closeGuards.delete(id);
    set((s) => {
      if (!s.tabs.some((t) => t.id === id)) return s;
      const { [id]: _h, ...hidden } = s.hidden;
      return { tabs: s.tabs.filter((t) => t.id !== id), hidden, ...withPanes(removeTab(panesOf(s), idsOf(s.tabs), id)) };
    });
  },
  closeOthers: async (id) => {
    const s0 = get();
    const others = paneTabs(idsOf(s0.tabs), s0, s0.paneOf[id]).filter((t) => t !== id);
    const doomed = new Set<string>();
    for (const t of others) {
      if (closeGuards.has(t)) get().activate(t); // show the tab being asked about
      if (await mayClose(t)) doomed.add(t);
    }
    doomed.forEach((d) => closeGuards.delete(d));
    set((s) => {
      let p = panesOf(s);
      let ids = idsOf(s.tabs);
      for (const d of doomed) {
        p = removeTab(p, ids, d);
        ids = ids.filter((x) => x !== d);
      }
      return { tabs: s.tabs.filter((t) => !doomed.has(t.id)), ...withPanes(activateTab(p, id)) };
    });
  },
  activate: (id, focus = true) => set((s) => withPanes(activateTab(panesOf(s), id, focus))),
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
  moveToPane: (id, pane, beforeId = null) => {
    const s = get();
    // in the tab order: before `beforeId`, or right after the pane's last tab
    const last = paneTabs(idsOf(s.tabs), s, pane).filter((t) => t !== id).pop();
    if (beforeId) get().move(id, beforeId);
    else if (last) {
      const after = s.tabs[s.tabs.findIndex((t) => t.id === last) + 1]?.id ?? null;
      if (after !== id) get().move(id, after);
    }
    set((s2) => withPanes(moveTab(panesOf(s2), idsOf(s2.tabs), id, pane)));
  },
  split: (id, side, pane) => set((s) => withPanes(splitTab(panesOf(s), idsOf(s.tabs), id, side, newPane(), pane ?? s.paneOf[id]))),
  focusPane: (pane) => set((s) => withPanes(focusPane(panesOf(s), pane))),
  setHidden: (ids) => {
    const cur = get().hidden;
    if (ids.length === Object.keys(cur).length && ids.every((i) => cur[i])) return;
    set({ hidden: Object.fromEntries(ids.map((i) => [i, true as const])) });
  },
}));

/** Convenience helpers used across features. */
export const openTab = (spec: TabSpec, title: string, key?: string) => useTabs.getState().open(spec, title, key);

const visibleIn = (s: TabsState, id: string) => s.paneActive[s.paneOf[id]] === id && !s.hidden[id];
/** Is the tab on screen (its pane's active tab, not hidden by focus mode)? With split panes several
 *  are; `activeId` is the one the user is in. For "refresh / fit / poll while shown". */
export const useTabVisible = (id: string) => useTabs((s) => visibleIn(s, id));
export const isTabVisible = (id: string) => visibleIn(useTabs.getState(), id);
