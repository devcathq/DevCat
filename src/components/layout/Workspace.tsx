import clsx from "clsx";
import { ArrowRightLeft, Columns2, Copy, CopyPlus, Pin, PinOff, Rows2, Sparkle, SquareArrowOutUpRight, SquareSplitHorizontal, SquareSplitVertical, X, Zap } from "lucide-react";
import { createContext, Fragment, useContext, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Group, Panel, Separator, type Layout } from "react-resizable-panels";
import { useShallow } from "zustand/react/shallow";
import { isPinned, pin, unpin, usePins } from "../../features/files/pins";
import { noteInFocus, useNotes } from "../../features/notes/store";
import { inFocus, useConnections } from "../../store/connections";
import { useContextMenu, type MenuItem } from "../../features/connections/ContextMenu";
import { confirmDialog } from "../../features/connections/dialogs";
import { detectSession } from "../../features/claude/store";
import { useTabActivity } from "../../features/claude/activity";
import { useQuickCommands } from "../../features/terminal/quickCommands";
import { openNewWindow, type TabTransfer } from "../../lib/windows";
import { paneKey, SPLIT_DOWN_SHORTCUT, SPLIT_RIGHT_SHORTCUT, type PaneKey } from "../../lib/paneKeys";
import { folderInFocus, useFolderProjects } from "../../store/folderProjects";
import { sessionIdOf } from "../../lib/remotePath";
import { openTab, useTabs, type Side } from "../../store/tabs";
import { isSplit, nextPane, paneIds, paneTabs, removeNode, type PaneNode } from "../../store/panes";
import { TabContent } from "../../tabs/registry";
import { TabIcon, tabKindLabel } from "../../tabs/TabIcon";
import { tabConnId, tabFolder, type Tab } from "../../tabs/types";
import { Spin } from "../ui";

// Split panes: each pane has its own tab bar and shows one of its tabs. Every tab's content is
// rendered once, here, into a detached host element of its own; the pane a tab is in puts that host
// into its content area. Moving a tab between panes or reshaping the tree only moves the host, so
// nothing unmounts (a terminal keeps its shell, an editor its unsaved text).

/** Each tab's host element, created once per tab id and removed when the tab closes. */
const hosts = new Map<string, HTMLDivElement>();
function hostOf(id: string): HTMLDivElement {
  let el = hosts.get(id);
  if (!el) {
    el = document.createElement("div");
    el.className = "absolute inset-0 outline-none";
    el.tabIndex = -1; // takes the keyboard when its tab has nothing better to focus
    el.dataset.tabHost = id;
    hosts.set(id, el);
  }
  return el;
}

/** Give the keyboard to a pane's active tab, the way a click in it would: a terminal takes the keys,
 *  an editor its cursor, anything else the tab's host element. */
function focusPaneContent(pane: string) {
  const s = useTabs.getState();
  const host = hosts.get(s.paneActive[pane] ?? "");
  if (!host || host.inert) return;
  const seen = (el: HTMLElement) => el.offsetParent !== null;
  const target = [...host.querySelectorAll<HTMLElement>(".xterm-helper-textarea, .monaco-editor textarea")].find(seen) ?? host;
  target.focus({ preventScroll: true });
}

/** The pane next to the focused one in a direction, by where the panes are on screen (no wrap-around). */
function neighbourPane(from: string, dir: "left" | "right" | "up" | "down"): string | null {
  const rects = new Map<string, DOMRect>();
  for (const el of document.querySelectorAll<HTMLElement>("[data-pane-slot]")) rects.set(el.dataset.paneSlot!, el.getBoundingClientRect());
  const cur = rects.get(from);
  if (!cur) return null;
  const horiz = dir === "left" || dir === "right";
  const eps = 2;
  let best: { pane: string; gap: number; off: number } | null = null;
  for (const [pane, r] of rects) {
    if (pane === from) continue;
    const gap = dir === "left" ? cur.left - r.right : dir === "right" ? r.left - cur.right : dir === "up" ? cur.top - r.bottom : r.top - cur.bottom;
    if (gap < -eps) continue;
    const overlap = horiz ? Math.min(cur.bottom, r.bottom) - Math.max(cur.top, r.top) : Math.min(cur.right, r.right) - Math.max(cur.left, r.left);
    if (overlap <= 0) continue;
    // nearest first; among equals the one most in line with the current pane
    const off = horiz ? Math.abs((r.top + r.bottom) / 2 - (cur.top + cur.bottom) / 2) : Math.abs((r.left + r.right) / 2 - (cur.left + cur.right) / 2);
    if (!best || gap < best.gap - eps || (Math.abs(gap - best.gap) <= eps && off < best.off)) best = { pane, gap, off };
  }
  return best?.pane ?? null;
}

/** A new tab like this one (same target), to put beside it. */
function copyOf(t: Tab) {
  const { id: _id, title, dirty: _d, ...spec } = t;
  return { spec, title };
}

/** Split the focused pane's active tab: a terminal / SFTP tab gets a copy beside it, any other tab
 *  moves into a new pane (nothing when it's its pane's only tab). */
function splitFocused(side: "right" | "bottom") {
  const s = useTabs.getState();
  const t = s.tabs.find((x) => x.id === s.paneActive[s.focusedPane]);
  if (!t) return;
  if (t.type === "terminal" || t.type === "sftp") {
    const { spec, title } = copyOf(t);
    s.openSplit(spec, title, t.id, side);
  } else if (paneTabs(s.tabs.map((x) => x.id), s, s.focusedPane).length >= 2) s.split(t.id, side);
}

function runPaneKey(k: PaneKey) {
  if (k === "splitRight" || k === "splitDown") splitFocused(k === "splitRight" ? "right" : "bottom");
  else {
    const to = neighbourPane(useTabs.getState().focusedPane, k);
    if (!to) return;
    useTabs.getState().focusPane(to);
  }
  // the panes may have been rebuilt (their content moved, the focus lost): give the keys to the focused one
  requestAnimationFrame(() => requestAnimationFrame(() => focusPaneContent(useTabs.getState().focusedPane)));
}

/** Sizes the user gave a split's panes, by the split's children (kept while the window is open). */
const sizes = new Map<string, Layout>();
const nodeId = (n: PaneNode) => (isSplit(n) ? n.split : n.pane);

/** Where a dragged tab would land: in a pane's tab bar (before a tab, null = after its last), or
 *  on a pane's content: an edge splits that pane, the middle moves the tab into it. */
type Drop = { pane: string; before: string | null } | { pane: string; side: Side | "center" };
type Drag = { id: string; drop: Drop | null };

interface Ctx {
  shown: Set<string>;
  hiddenCount: number;
  firstPane: string | null;
  panes: number;
  drag: Drag | null;
  startDrag: (e: React.MouseEvent, id: string) => void;
  tabMenu: (e: React.MouseEvent, t: Tab) => void;
}
const WorkspaceCtx = createContext<Ctx>(null!);

export function Workspace() {
  const allTabs = useTabs((s) => s.tabs);
  const layout = useTabs((s) => s.layout);
  const paneOf = useTabs((s) => s.paneOf);
  const menu = useContextMenu();

  // Focus mode hides tabs of other projects' connections, notes and folders (chats, terminals,
  // files, Git): they stay mounted and come back on exit. A folder on an SSH session goes by that
  // session's project, so its chat and terminal show together.
  const connOf = (t: Tab) => tabConnId(t) ?? sessionIdOf(tabFolder(t));
  const focusId = useConnections((s) => s.focusProjectId);
  const connections = useConnections((s) => s.connections);
  const notes = useNotes((s) => s.notes);
  const folders = useFolderProjects((s) => s.folders);
  const shown = useMemo(
    () => new Set(allTabs.filter((t) => t.type === "note"
      ? noteInFocus(notes.find((n) => n.id === t.noteId)?.projectId)
      : connOf(t) ? inFocus(connOf(t)) : folderInFocus(tabFolder(t))).map((t) => t.id)),
    [allTabs, focusId, connections, notes, folders], // eslint-disable-line react-hooks/exhaustive-deps
  );
  const hiddenCount = allTabs.length - shown.size;
  useLayoutEffect(() => useTabs.getState().setHidden(allTabs.filter((t) => !shown.has(t.id)).map((t) => t.id)), [allTabs, shown]);

  // A pane whose tab focus mode hides shows another of its own; the focus leaves a pane with nothing to show.
  const paneActive = useTabs((s) => s.paneActive);
  const focusedPane = useTabs((s) => s.focusedPane);
  useEffect(() => {
    const s = useTabs.getState();
    const ids = s.tabs.map((t) => t.id);
    const vis = (pane: string) => paneTabs(ids, s, pane).filter((t) => shown.has(t));
    for (const pane of paneIds(s.layout)) {
      const v = vis(pane);
      const a = s.paneActive[pane];
      if (v.length && (!a || !shown.has(a))) s.activate(v[v.length - 1], false);
    }
    if (!vis(s.focusedPane).length) {
      const other = paneIds(s.layout).find((p) => vis(p).length);
      if (other) s.focusPane(other);
    }
  }, [shown, layout, paneOf, paneActive, focusedPane]);

  // Alt+Shift+⌘/Ctrl + \ - arrows: split, move between panes. Capture phase, before a terminal sees the key.
  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      const k = paneKey(e);
      if (!k) return;
      e.preventDefault(); // AltGr layouts would type a character
      e.stopPropagation();
      if (!e.repeat) runPaneKey(k);
    };
    window.addEventListener("keydown", h, true);
    return () => window.removeEventListener("keydown", h, true);
  }, []);

  // Closed tabs: drop their hosts (their content has unmounted by now).
  useEffect(() => {
    const open = new Set(allTabs.map((t) => t.id));
    for (const [id, el] of hosts) if (!open.has(id)) { el.remove(); hosts.delete(id); }
  }, [allTabs]);

  // Panes with nothing to show in focus mode are left out (their tabs stay mounted, off screen).
  // An empty pane is kept only when it's the last one.
  const visibleLayout = useMemo(() => {
    let n: PaneNode | null = layout;
    for (const p of paneIds(layout)) {
      const own = allTabs.filter((t) => paneOf[t.id] === p);
      if (own.length && !own.some((t) => shown.has(t.id))) n = n && removeNode(n, p);
    }
    return n ?? { pane: focusedPane };
  }, [layout, allTabs, paneOf, shown, focusedPane]);

  // Reshaping the pane tree remounts the panes and moves the hosts, which drops the keyboard focus (a
  // terminal being typed in goes deaf): give it back to the focused pane when it ended up nowhere.
  const treeKey = JSON.stringify(visibleLayout);
  const firstTree = useRef(true);
  useEffect(() => {
    if (firstTree.current) { firstTree.current = false; return; }
    const raf = requestAnimationFrame(() => {
      if (!document.activeElement || document.activeElement === document.body) focusPaneContent(useTabs.getState().focusedPane);
    });
    return () => cancelAnimationFrame(raf);
  }, [treeKey]);

  // Keep every tab mounted so terminals, editors and grids keep their state. (Not re-rendered by drags or focus changes.)
  const contents = useMemo(() => allTabs.map((t) => createPortal(<TabContent tab={t} />, hostOf(t.id), t.id)), [allTabs]);
  const { drag, startDrag } = useTabDrag();
  const tabMenu = (e: React.MouseEvent, t: Tab) => menu.show(e, tabMenuItems(t));
  const ctx: Ctx = { shown, hiddenCount, firstPane: paneIds(visibleLayout)[0] ?? null, panes: paneIds(layout).length, drag, startDrag, tabMenu };

  return (
    <div className="flex h-full flex-col bg-panel">
      <WorkspaceCtx.Provider value={ctx}>
        <PaneTree node={visibleLayout} />
      </WorkspaceCtx.Provider>
      {menu.element}
      {contents}
    </div>
  );
}

function PaneTree({ node }: { node: PaneNode }) {
  if (!isSplit(node)) return <PaneView id={node.pane} />;
  const key = node.children.map(nodeId).join(",");
  const row = node.dir === "row";
  return (
    <Group
      key={key}
      orientation={row ? "horizontal" : "vertical"}
      className="h-full w-full"
      defaultLayout={sizes.get(key)}
      onLayoutChanged={(l) => sizes.set(key, l)}
    >
      {node.children.map((c, i) => (
        <Fragment key={nodeId(c)}>
          {i > 0 && <Separator className={clsx(row ? "w-px" : "h-px", "bg-line hover:bg-accent data-[separator=active]:bg-accent")} />}
          <Panel id={nodeId(c)} minSize="8%" className="min-h-0 min-w-0">
            <PaneTree node={c} />
          </Panel>
        </Fragment>
      ))}
    </Group>
  );
}

function PaneView({ id }: { id: string }) {
  const { shown, drag } = useContext(WorkspaceCtx);
  const tabIds = useTabs(useShallow((s) => paneTabs(s.tabs.map((t) => t.id), s, id)));
  const active = useTabs((s) => s.paneActive[id] ?? null);
  const slot = useRef<HTMLDivElement>(null);

  // This pane's tabs live in its content area; only the active one shows. A hidden tab isn't
  // `display: none`: that drops its layout, and with it every scroll position (an HTML preview's
  // frame, a long file) once the tab shows again. `content-visibility: hidden` keeps the layout but
  // skips its rendering work (animations included), `invisible` is the fallback where it isn't
  // supported, and `inert` takes focus and clicks away from it.
  useLayoutEffect(() => {
    const el = slot.current!;
    for (const t of tabIds) {
      const h = hostOf(t);
      if (h.parentElement !== el) el.appendChild(h);
      const hidden = t !== active || !shown.has(t);
      h.inert = hidden;
      h.classList.toggle("invisible", hidden);
      h.classList.toggle("[content-visibility:hidden]", hidden);
    }
  });

  // A click anywhere in the pane (a terminal too) makes it the focused one. Native, in the capture
  // phase: the content is portalled, so React's events don't pass through this element.
  useEffect(() => {
    const el = slot.current!;
    const focus = () => { if (useTabs.getState().focusedPane !== id) useTabs.getState().focusPane(id); };
    el.addEventListener("mousedown", focus, true);
    return () => el.removeEventListener("mousedown", focus, true);
  }, [id]);

  const zone = drag?.drop && "side" in drag.drop && drag.drop.pane === id ? drag.drop.side : null;
  return (
    <div className="flex h-full min-h-0 min-w-0 flex-col">
      <TabBar pane={id} />
      <div className="relative min-h-0 flex-1">
        <div ref={slot} data-pane-slot={id} className="absolute inset-0" />
        {zone && (
          <div className={clsx(
            "pointer-events-none absolute z-20 rounded-sm border border-accent/60 bg-accent/15",
            zone === "left" ? "inset-y-0 left-0 w-1/2" : zone === "right" ? "inset-y-0 right-0 w-1/2" : zone === "bottom" ? "inset-x-0 bottom-0 h-1/2" : "inset-0",
          )} />
        )}
      </div>
    </div>
  );
}

function TabBar({ pane }: { pane: string }) {
  const { shown, hiddenCount, firstPane, panes, drag, startDrag, tabMenu } = useContext(WorkspaceCtx);
  const allTabs = useTabs((s) => s.tabs);
  const paneOf = useTabs((s) => s.paneOf);
  const active = useTabs((s) => s.paneActive[pane] ?? null);
  const focused = useTabs((s) => s.focusedPane === pane);
  const { activate, close } = useTabs.getState();
  const tabs = allTabs.filter((t) => paneOf[t.id] === pane && shown.has(t.id));
  const connections = useConnections((s) => s.connections);
  const connName = (t: Tab) => { const id = tabConnId(t); return id ? connections.find((c) => c.id === id)?.name : undefined; };
  const connColor = (t: Tab) => { const id = tabConnId(t); return id ? connections.find((c) => c.id === id)?.color : undefined; };
  const target = drag?.drop && "before" in drag.drop && drag.drop.pane === pane ? drag.drop : null;
  const first = pane === firstPane;
  // Bring the active tab into view when it changes (a new tab opens at the end of a full bar).
  // Only the bar scrolls: scrollIntoView would also move the containers around it.
  const barRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const bar = barRef.current;
    const el = bar?.querySelector<HTMLElement>(`[data-tab-id="${CSS.escape(active ?? "")}"]`);
    if (!bar || !el) return;
    const b = bar.getBoundingClientRect(), r = el.getBoundingClientRect();
    if (r.left < b.left) bar.scrollLeft -= b.left - r.left;
    else if (r.right > b.left + bar.clientWidth) bar.scrollLeft += r.right - (b.left + bar.clientWidth);
  }, [active, tabs.length]);
  return (
    <div
      ref={barRef}
      data-pane-bar={pane}
      data-tour={first ? "tabs" : undefined}
      onMouseDown={() => useTabs.getState().focusPane(pane)}
      className="no-select flex h-9 shrink-0 items-stretch overflow-x-auto border-b border-line bg-app"
    >
      {tabs.map((t, i) => (
        <div
          key={t.id}
          data-tab-id={t.id}
          onMouseDown={(e) => {
            if (e.button === 1) { e.preventDefault(); close(t.id); return; }
            activate(t.id);
            if (e.button === 0) startDrag(e, t.id);
          }}
          onContextMenu={(e) => tabMenu(e, t)}
          title={drag ? undefined : tabKindLabel(t, connName(t))}
          className={clsx(
            "group relative flex max-w-56 shrink-0 cursor-default items-center gap-2 border-r border-line pl-3 pr-1.5",
            t.id === active
              // the accent line marks where the user is; other panes' shown tabs get a faint one
              ? clsx("bg-panel text-fg", focused || panes < 2 ? "shadow-[inset_0_2px_0_var(--color-accent)]" : "shadow-[inset_0_2px_0_var(--color-faint)]")
              : "text-muted hover:bg-panel/60",
            drag?.id === t.id && "opacity-50",
          )}
        >
          {/* Where the dragged tab lands: before this one (or after the last). */}
          {target && target.before === t.id && drag!.id !== t.id && tabs[i - 1]?.id !== drag!.id && (
            <span className="pointer-events-none absolute inset-y-1 -left-px w-0.5 rounded bg-accent" />
          )}
          {target && target.before === null && i === tabs.length - 1 && drag!.id !== t.id && (
            <span className="pointer-events-none absolute inset-y-1 -right-px w-0.5 rounded bg-accent" />
          )}
          {/* The connection's color (e.g. red for production) under its tabs. */}
          {connColor(t) && <span className="pointer-events-none absolute inset-x-2 bottom-0 h-0.5 rounded-t" style={{ background: connColor(t)! }} />}
          <TabIcon tab={t} />
          {t.type === "claude" ? <AgentTabTitle tabId={t.id} title={t.title} /> : <span className="truncate" title={t.title}>{t.title}</span>}
          {t.dirty && <span className="size-1.5 rounded-full bg-accent" />}
          <button
            aria-label="Close tab"
            onMouseDown={(e) => e.stopPropagation()}
            onClick={() => close(t.id)}
            className="rounded p-0.5 opacity-0 hover:bg-hover group-hover:opacity-100"
          >
            <X className="size-3.5" />
          </button>
        </div>
      ))}
      {/* An empty bar of another pane: the dragged tab goes at its start. */}
      {target && !tabs.length && <span className="pointer-events-none my-1 ml-1 w-0.5 rounded bg-accent" />}
      {first && hiddenCount > 0 && (
        <span className="ml-auto flex shrink-0 items-center px-3 text-[11px] text-faint" title="Tabs from other projects are hidden while focus mode is on">
          {hiddenCount} tab{hiddenCount === 1 ? "" : "s"} hidden by focus
        </span>
      )}
    </div>
  );
}

function tabMenuItems(t: Tab): MenuItem[] {
  const s = useTabs.getState();
  const focusId = useConnections.getState().focusProjectId;
  const transferable = t.type !== "welcome";
  const pane = s.paneOf[t.id];
  const inPane = paneTabs(s.tabs.map((x) => x.id), s, pane).length;
  const next = nextPane(s.layout, pane);
  const dup = t.type === "terminal" || t.type === "sftp";
  const copySpec = () => copyOf(t);
  const kind = t.type === "terminal" ? "terminal" : "SFTP";
  return [
    // a ghost chat belongs to its window: closing that window deletes it
    { label: "Move to new window", icon: <SquareArrowOutUpRight className="size-3.5" />, disabled: !transferable || (t.type === "claude" && !!t.ghost), onClick: () => void moveTabToWindow(t) },
    // A copy of an AI workspace tab would run a second agent on the same session.
    ...(t.type !== "claude"
      ? [{ label: "Open copy in new window", icon: <Copy className="size-3.5" />, disabled: !transferable, onClick: async () => void openNewWindow(focusId, await tabTransfer(t)) } as MenuItem]
      : []),
    // Another shell / file browser on the same host (Xshell's duplicate session).
    ...(dup
      ? [{ label: "Duplicate tab", icon: <CopyPlus className="size-3.5" />, onClick: () => { const { spec, title } = copySpec(); openTab(spec, title); } } as MenuItem]
      : []),
    // pinned files show at the top of the AI workspace's Files (features/files/pins.ts)
    ...(t.type === "file"
      ? [isPinned(usePins.getState().pins, t.path)
        ? { label: "Unpin file", icon: <PinOff className="size-3.5" />, onClick: () => unpin(t.path) } as MenuItem
        : { label: "Pin file", icon: <Pin className="size-3.5" />, onClick: () => pin(t.path, t.root ?? null) } as MenuItem]
      : []),
    ...((t.type === "terminal" || t.type === "claude") && !useQuickCommands.getState().visible
      ? [{ label: "Show quick command bar", icon: <Zap className="size-3.5" />, onClick: () => useQuickCommands.getState().setVisible(true) } as MenuItem]
      : []),
    "separator",
    // A terminal split: a new shell on the same target next to this one.
    ...(dup
      ? [
          { label: `Split ${kind} right`, shortcut: SPLIT_RIGHT_SHORTCUT, icon: <SquareSplitHorizontal className="size-3.5" />, onClick: () => { const { spec, title } = copySpec(); useTabs.getState().openSplit(spec, title, t.id, "right"); } } as MenuItem,
          { label: `Split ${kind} down`, shortcut: SPLIT_DOWN_SHORTCUT, icon: <SquareSplitVertical className="size-3.5" />, onClick: () => { const { spec, title } = copySpec(); useTabs.getState().openSplit(spec, title, t.id, "bottom"); } } as MenuItem,
        ]
      : []),
    // the tab itself into a new pane (a pane's only tab has nowhere to go from its own pane)
    { label: "Split right", shortcut: SPLIT_RIGHT_SHORTCUT, icon: <Columns2 className="size-3.5" />, disabled: inPane < 2, onClick: () => useTabs.getState().split(t.id, "right") },
    { label: "Split down", shortcut: SPLIT_DOWN_SHORTCUT, icon: <Rows2 className="size-3.5" />, disabled: inPane < 2, onClick: () => useTabs.getState().split(t.id, "bottom") },
    ...(next
      ? [{ label: "Move to next pane", icon: <ArrowRightLeft className="size-3.5" />, onClick: () => useTabs.getState().moveToPane(t.id, next) } as MenuItem]
      : []),
    "separator",
    { label: "Close", icon: <X className="size-3.5" />, onClick: () => s.close(t.id) },
    { label: "Close others", disabled: inPane < 2, onClick: () => s.closeOthers(t.id) },
  ];
}

/** AI workspace tab title: pulsing "working" while the agent runs, a dot when it finished unseen. */
function AgentTabTitle({ tabId, title }: { tabId: string; title: string }) {
  const { working, done } = useTabActivity(tabId);
  return (
    <>
      {working && <Spin icon={Sparkle} className="size-3 shrink-0 text-accent [animation-duration:2.4s]" />}
      <span className="flex min-w-0 flex-col leading-tight">
        <span className="truncate">{title}</span>
        {working && <span className="working-text text-[10px] font-medium">working</span>}
      </span>
      {done && !working && <span className="size-1.5 shrink-0 rounded-full bg-ok" title="Finished" />}
    </>
  );
}

/** Spec + title of a tab, to recreate it in another window. */
async function tabTransfer(t: Tab): Promise<TabTransfer> {
  const { id, dirty, ...spec } = t;
  // AI workspace on Claude: resume the session the terminal is on (it may have changed since the tab opened).
  if (spec.type === "claude") return { ...spec, sessionId: (await detectSession(id)) ?? spec.sessionId ?? null };
  return spec;
}

async function moveTabToWindow(t: Tab) {
  if (t.dirty && t.type !== "query") {
    const ok = await confirmDialog({
      title: "Move tab",
      message: <>The tab <b>{t.title}</b> has unsaved changes, which are not moved to the new window. Move it anyway?</>,
      confirmLabel: "Move",
      danger: true,
    });
    if (!ok) return;
  }
  if (await openNewWindow(useConnections.getState().focusProjectId, await tabTransfer(t))) useTabs.getState().close(t.id, true);
}

/** Drag tabs along a tab bar to reorder them, onto another pane's bar to move them there, or onto
 *  a pane's content: its left / right / bottom edge splits it, the middle moves the tab into it.
 *  Mouse events: Tauri's file drop handling gets in the way of HTML drag and drop. A bar scrolls
 *  when dragging near its edges. */
function useTabDrag() {
  const [drag, setDrag] = useState<Drag | null>(null);
  const startDrag = (e: React.MouseEvent, id: string) => {
    const x0 = e.clientX;
    const y0 = e.clientY;
    let moving = false;
    const s0 = useTabs.getState();
    const from = s0.paneOf[id];
    const alone = paneTabs(s0.tabs.map((t) => t.id), s0, from).length < 2;
    const inside = (r: DOMRect, x: number, y: number, slackY = 0) => x >= r.left && x <= r.right && y >= r.top - slackY && y <= r.bottom + slackY;
    const target = (x: number, y: number): Drop | null => {
      for (const bar of document.querySelectorAll<HTMLElement>("[data-pane-bar]")) {
        // a little below the bar still counts: a drag along it drifts
        if (!inside(bar.getBoundingClientRect(), x, y, 12)) continue;
        let before: string | null = null;
        for (const el of bar.querySelectorAll<HTMLElement>("[data-tab-id]")) {
          const r = el.getBoundingClientRect();
          if (x < r.left + r.width / 2) { before = el.dataset.tabId ?? null; break; }
        }
        return { pane: bar.dataset.paneBar!, before };
      }
      for (const slot of document.querySelectorAll<HTMLElement>("[data-pane-slot]")) {
        const r = slot.getBoundingClientRect();
        if (!inside(r, x, y)) continue;
        const fx = (x - r.left) / r.width;
        const fy = (y - r.top) / r.height;
        const side = fx < 0.25 ? "left" : fx > 0.75 ? "right" : fy > 0.7 ? "bottom" : "center";
        const pane = slot.dataset.paneSlot!;
        // its own pane: the middle changes nothing, and a lone tab can't split off it
        if (pane === from && (side === "center" || alone)) return null;
        return { pane, side };
      }
      return null;
    };
    const onMove = (ev: MouseEvent) => {
      if (!moving && Math.abs(ev.clientX - x0) < 5 && Math.abs(ev.clientY - y0) < 5) return;
      moving = true;
      const drop = target(ev.clientX, ev.clientY);
      if (drop && "before" in drop) {
        const bar = document.querySelector<HTMLElement>(`[data-pane-bar="${drop.pane}"]`);
        const r = bar?.getBoundingClientRect();
        if (bar && r && ev.clientX < r.left + 24) bar.scrollLeft -= 16;
        else if (bar && r && ev.clientX > r.right - 24) bar.scrollLeft += 16;
      }
      setDrag((d) => (d && d.id === id && JSON.stringify(d.drop) === JSON.stringify(drop) ? d : { id, drop }));
    };
    const onUp = (ev: MouseEvent) => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      setDrag(null);
      if (!moving) return;
      const drop = target(ev.clientX, ev.clientY);
      const tabs = useTabs.getState();
      if (!drop) return;
      if ("before" in drop) {
        if (drop.pane === from) tabs.move(id, drop.before);
        else tabs.moveToPane(id, drop.pane, drop.before);
      } else if (drop.side === "center") tabs.moveToPane(id, drop.pane);
      else tabs.split(id, drop.side, drop.pane);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };
  return { drag, startDrag };
}
