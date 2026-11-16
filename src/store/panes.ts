// Split panes of the workspace: a tree of splits whose leaves are panes, which pane each tab is in,
// each pane's active tab and the focused pane. Tabs keep their order in the flat tab list; a pane
// shows its own in that order. Pure functions, no React / zustand (`pnpm check:panes`).

/** row: side by side; col: one above the other. */
export type SplitDir = "row" | "col";
export type Side = "left" | "right" | "top" | "bottom";
export type PaneNode = { pane: string } | { split: string; dir: SplitDir; children: PaneNode[] };
type SplitNode = Extract<PaneNode, { split: string }>;

export interface Panes {
  layout: PaneNode;
  /** tab id → pane id */
  paneOf: Record<string, string>;
  /** pane id → the tab it shows */
  paneActive: Record<string, string | null>;
  focusedPane: string;
}

export const isSplit = (n: PaneNode): n is SplitNode => "split" in n;

export function initialPanes(pane: string, tabs: readonly string[]): Panes {
  return { layout: { pane }, paneOf: Object.fromEntries(tabs.map((t) => [t, pane])), paneActive: { [pane]: tabs[tabs.length - 1] ?? null }, focusedPane: pane };
}

/** Pane ids, left to right / top to bottom. */
export function paneIds(n: PaneNode): string[] {
  return isSplit(n) ? n.children.flatMap(paneIds) : [n.pane];
}

/** The tabs of a pane, in tab order (`ids`: every tab id, in order). */
export function paneTabs(ids: readonly string[], p: Panes, pane: string): string[] {
  return ids.filter((id) => p.paneOf[id] === pane);
}

/** The pane after `pane` (wrapping), or null when there's only one. */
export function nextPane(layout: PaneNode, pane: string): string | null {
  const all = paneIds(layout);
  if (all.length < 2) return null;
  return all[(all.indexOf(pane) + 1) % all.length];
}

/** Put a new pane `fresh` on `side` of `target`. Joins `target`'s split when it runs the same way. */
export function splitNode(n: PaneNode, target: string, fresh: string, side: Side): PaneNode {
  const dir: SplitDir = side === "left" || side === "right" ? "row" : "col";
  const after = side === "right" || side === "bottom";
  const leaf = { pane: fresh };
  if (!isSplit(n)) return n.pane === target ? { split: `s-${fresh}`, dir, children: after ? [n, leaf] : [leaf, n] } : n;
  const at = n.dir === dir ? n.children.findIndex((c) => !isSplit(c) && c.pane === target) : -1;
  if (at >= 0) return { ...n, children: [...n.children.slice(0, at + (after ? 1 : 0)), leaf, ...n.children.slice(at + (after ? 1 : 0))] };
  return { ...n, children: n.children.map((c) => splitNode(c, target, fresh, side)) };
}

/** Take a pane out; splits left with one child give way to it, and a split inside one running the
 *  same way merges into it. Null when nothing is left. */
export function removeNode(n: PaneNode, pane: string): PaneNode | null {
  if (!isSplit(n)) return n.pane === pane ? null : n;
  const kids = n.children.map((c) => removeNode(c, pane)).filter((c): c is PaneNode => c !== null);
  if (!kids.length) return null;
  if (kids.length === 1) return kids[0];
  return { ...n, children: kids.flatMap((c) => (isSplit(c) && c.dir === n.dir ? c.children : [c])) };
}

/** A tab joins `pane` (or the focused one) as its active tab, and that pane gets the focus. */
export function placeTab(p: Panes, tab: string, pane = p.focusedPane): Panes {
  return { ...p, paneOf: { ...p.paneOf, [tab]: pane }, paneActive: { ...p.paneActive, [pane]: tab }, focusedPane: pane };
}

/** Show a tab in its pane; `focus` also makes that pane the focused one. */
export function activateTab(p: Panes, tab: string, focus = true): Panes {
  const pane = p.paneOf[tab];
  if (!pane) return p;
  return { ...p, paneActive: { ...p.paneActive, [pane]: tab }, focusedPane: focus ? pane : p.focusedPane };
}

export function focusPane(p: Panes, pane: string): Panes {
  return paneIds(p.layout).includes(pane) ? { ...p, focusedPane: pane } : p;
}

/** A tab leaves its pane (closed, or on its way to another). Its neighbour in the pane takes
 *  over; a pane left empty goes away, unless it's the last one. `ids`: tab order, with `tab`. */
export function removeTab(p: Panes, ids: readonly string[], tab: string): Panes {
  const pane = p.paneOf[tab];
  if (!pane) return p;
  const { [tab]: _gone, ...paneOf } = p.paneOf;
  const siblings = paneTabs(ids, p, pane);
  const rest = siblings.filter((t) => t !== tab);
  if (rest.length) {
    const active = p.paneActive[pane] === tab ? rest[Math.min(siblings.indexOf(tab), rest.length - 1)] : p.paneActive[pane];
    return { ...p, paneOf, paneActive: { ...p.paneActive, [pane]: active } };
  }
  const all = paneIds(p.layout);
  if (all.length < 2) return { ...p, paneOf, paneActive: { ...p.paneActive, [pane]: null } };
  const i = all.indexOf(pane);
  const near = all[i > 0 ? i - 1 : i + 1];
  const { [pane]: _p, ...paneActive } = p.paneActive;
  return { layout: removeNode(p.layout, pane)!, paneOf, paneActive, focusedPane: p.focusedPane === pane ? near : p.focusedPane };
}

/** Move a tab into another pane (as its active tab, focused). */
export function moveTab(p: Panes, ids: readonly string[], tab: string, to: string): Panes {
  if (!p.paneOf[tab] || !paneIds(p.layout).includes(to)) return p;
  if (p.paneOf[tab] === to) return activateTab(p, tab);
  return placeTab(removeTab(p, ids, tab), tab, to);
}

/** Move a tab into a new pane `fresh` on `side` of `target` (by default its own pane). Nothing
 *  happens for a pane's only tab split off its own pane. */
export function splitTab(p: Panes, ids: readonly string[], tab: string, side: Side, fresh: string, target = p.paneOf[tab]): Panes {
  const from = p.paneOf[tab];
  if (!from || !paneIds(p.layout).includes(target)) return p;
  if (target === from && paneTabs(ids, p, from).length < 2) return p;
  return moveTab({ ...p, layout: splitNode(p.layout, target, fresh, side), paneActive: { ...p.paneActive, [fresh]: null } }, ids, tab, fresh);
}

/** A new tab, in a new pane `fresh` on `side` of `target`. */
export function openBeside(p: Panes, tab: string, side: Side, fresh: string, target = p.focusedPane): Panes {
  if (!paneIds(p.layout).includes(target)) return placeTab(p, tab);
  return placeTab({ ...p, layout: splitNode(p.layout, target, fresh, side) }, tab, fresh);
}
