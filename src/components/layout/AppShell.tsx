import clsx from "clsx";
import { getVersion } from "@tauri-apps/api/app";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { AppWindow, ArrowUpCircle, Cable, Sparkle, ChevronDown, Copy, CopyPlus, Crosshair, Folder, Layers, FolderPlus, GitBranch, History, LogOut, NotebookPen, PanelLeftClose, PanelLeftOpen, Plus, Shield, SquareArrowOutUpRight, X, Zap } from "lucide-react";
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { Group, Panel, Separator, usePanelRef } from "react-resizable-panels";
import { Toaster } from "sonner";
import ConnectionDialog from "../../features/connections/ConnectionDialog";
import ConnectionTree from "../../features/connections/ConnectionTree";
import HistoryPanel from "../../features/history/HistoryPanel";
import NetworkPanel from "../../features/network/NetworkPanel";
import ClaudePanel from "../../features/claude/ClaudePanel";
import { AiConnectionsDialog, ProviderPicker } from "../../features/claude/AiProviders";
import GitPanel from "../../features/git/GitPanel";
import NotesPanel from "../../features/notes/NotesPanel";
import { createNote, noteInFocus, useNotes } from "../../features/notes/store";
import { inFocus, useConnections, useFocusProject } from "../../store/connections";
import { useContextMenu, type MenuItem } from "../../features/connections/ContextMenu";
import { createProject, focusProject } from "../../features/connections/tree/projectActions";
import { confirmDialog } from "../../features/connections/dialogs";
import { detectSession } from "../../features/claude/store";
import { useTabActivity } from "../../features/claude/activity";
import { KeepAwakeButton } from "../../features/keepawake/KeepAwakeButton";
import { useQuickCommands } from "../../features/terminal/quickCommands";
import { UpdateDialog } from "../../features/update/UpdateDialog";
import { MobileButton, MobileDialog } from "../../features/mobile/MobileDialog";
import { RoomJoinPrompt, RoomsButton, RoomsDialog } from "../../features/rooms/RoomsDialog";
import { TicketsButton } from "../../features/tickets/TicketsButton";
import { AppearanceButton, AppearanceDialog } from "../../features/appearance/AppearanceDialog";
import { MemoryButton, MemoryDialog } from "../../features/memory/MemoryDialog";
import { MemoryPairPrompt } from "../../features/memory/OtherComputers";
import { useAppearance, useIsDark } from "../../features/appearance/store";
import { zoomUi } from "../../features/appearance/fonts";
import { PairPrompt } from "../../features/mobile/PairPrompt";
import { startMobileStatus } from "../../features/mobile/store";
import { startUpdateChecks, useUpdate } from "../../features/update/store";
import { IS_MAC, IS_MAIN_WINDOW, NEW_WINDOW_SHORTCUT, openNewWindow, takeLaunchTab, type TabTransfer } from "../../lib/windows";
import "../../store/projectFolders";
import "../../features/files/openedFiles";
import { openQuickOpen, QuickOpen, useQuickOpen } from "../../features/files/QuickOpen";
import { openTab, useTabs } from "../../store/tabs";
import { useUi, type SidebarView } from "../../store/ui";
import { TabContent } from "../../tabs/registry";
import { TabIcon, tabKindLabel } from "../../tabs/TabIcon";
import { tabConnId, type Tab } from "../../tabs/types";
import { IconButton } from "../ui";
import { AiTerminalIcon } from "../ui/AppIcons";

export default function AppShell() {
  const load = useConnections((s) => s.load);
  useEffect(() => { load().catch(console.error); }, [load]);

  // Opened as "move / open tab in new window": show that tab.
  useEffect(() => {
    const t = takeLaunchTab();
    if (t) {
      const { title, ...spec } = t;
      openTab(spec, title);
    }
  }, []);

  // ⌘⇧N / Ctrl+Shift+N: new window. On macOS the app menu's "New Window" item owns the shortcut.
  useEffect(() => {
    if (IS_MAC) return;
    const h = (e: KeyboardEvent) => {
      if (e.ctrlKey && e.shiftKey && !e.altKey && !e.metaKey && e.key.toLowerCase() === "n") {
        e.preventDefault();
        void openNewWindow();
      }
    };
    window.addEventListener("keydown", h, true);
    return () => window.removeEventListener("keydown", h, true);
  }, []);

  useEffect(() => startMobileStatus(), []);
  // A project's own look (Appearance → This project) follows the window's focus.
  const focusId = useConnections((s) => s.focusProjectId);
  useEffect(() => useAppearance.getState().setFocus(focusId), [focusId]);
  const toastTheme = useIsDark() ? "dark" : "light";

  // Sidebar hide / show: the store is the source of truth; the panel follows it.
  const sidebarPanel = usePanelRef();
  const collapsed = useUi((s) => s.sidebarCollapsed);
  useEffect(() => {
    const p = sidebarPanel.current;
    if (!p) return;
    if (collapsed && !p.isCollapsed()) p.collapse();
    else if (!collapsed && p.isCollapsed()) {
      p.expand();
      // started collapsed: there's no earlier width to go back to, so use the default
      if (p.getSize().asPercentage < 18) p.resize("22%");
    }
  }, [collapsed, sidebarPanel]);
  // ⌘B / Ctrl+B. Not inside a terminal on Windows/Linux, where Ctrl+B belongs to the shell (tmux, readline).
  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      const mod = IS_MAC ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
      if (!mod || e.shiftKey || e.altKey || e.key.toLowerCase() !== "b") return;
      if (!IS_MAC && (e.target as HTMLElement | null)?.closest?.(".xterm")) return;
      e.preventDefault();
      const ui = useUi.getState();
      ui.setSidebarCollapsed(!ui.sidebarCollapsed);
    };
    window.addEventListener("keydown", h, true);
    return () => window.removeEventListener("keydown", h, true);
  }, []);

  // ⌘P / Ctrl+P: Go to File (instead of the webview's Print). In a terminal on Windows / Linux,
  // Ctrl+P stays with the shell (previous command).
  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      const mod = IS_MAC ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
      if (!mod || e.shiftKey || e.altKey || e.key.toLowerCase() !== "p") return;
      if (!IS_MAC && (e.target as HTMLElement | null)?.closest?.(".xterm")) return;
      e.preventDefault();
      // while it's open, Ctrl+P is the palette's own "move up" (Windows / Linux)
      if (useQuickOpen.getState().open) return;
      e.stopPropagation();
      openQuickOpen();
    };
    window.addEventListener("keydown", h, true);
    return () => window.removeEventListener("keydown", h, true);
  }, []);

  // ⌘/Ctrl + = / - / 0: UI zoom. Inside a terminal the same keys change the terminal font size instead.
  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      const mod = IS_MAC ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
      if (!mod || e.altKey) return;
      const step = e.code === "Equal" || e.code === "NumpadAdd" ? 1 : e.code === "Minus" || e.code === "NumpadSubtract" ? -1 : e.code === "Digit0" || e.code === "Numpad0" ? 0 : null;
      if (step === null || (e.target as HTMLElement | null)?.closest?.(".xterm")) return;
      e.preventDefault();
      zoomUi(step);
    };
    window.addEventListener("keydown", h, true);
    return () => window.removeEventListener("keydown", h, true);
  }, []);

  return (
    <div className="flex h-full flex-col">
      <ProjectBar />
      <div className="flex min-h-0 flex-1">
        <ActivityBar />
        <Group orientation="horizontal" className="min-w-0 flex-1">
          <Panel
            className="glass-side"
            panelRef={sidebarPanel}
            defaultSize="22%"
            minSize="14%"
            maxSize="45%"
            collapsible
            collapsedSize="0px"
            onResize={() => {
              // dragging the divider all the way left collapses it too
              const c = sidebarPanel.current?.isCollapsed() ?? false;
              if (c !== useUi.getState().sidebarCollapsed) useUi.getState().setSidebarCollapsed(c);
            }}
          >
            <Sidebar />
          </Panel>
          <Separator className="w-px bg-line hover:bg-accent data-[separator=active]:bg-accent" />
          <Panel minSize="40%" className="glass-work">
            <Workspace />
          </Panel>
        </Group>
      </div>
      <StatusBar />
      <ConnectionDialog />
      <AiConnectionsDialog />
      <UpdateDialog />
      <MobileDialog />
      <RoomsDialog />
      <RoomJoinPrompt />
      <AppearanceDialog />
      <MemoryDialog />
      <MemoryPairPrompt />
      <PairPrompt />
      <QuickOpen />
      <Toaster theme={toastTheme} position="bottom-right" richColors closeButton />
    </div>
  );
}

function ActivityBar() {
  const { sidebar, setSidebar, sidebarCollapsed, setSidebarCollapsed } = useUi();
  const toggleKey = IS_MAC ? "⌘B" : "Ctrl+B";
  // Clicking the active view hides / shows the sidebar (like VS Code); another view opens it.
  const pick = (v: SidebarView) => {
    if (v === sidebar) setSidebarCollapsed(!sidebarCollapsed);
    else { setSidebar(v); setSidebarCollapsed(false); }
  };
  const item = (v: SidebarView, icon: ReactNode, title: string) => (
    <button
      title={sidebar === v ? `${title} (click to ${sidebarCollapsed ? "show" : "hide"} the sidebar, ${toggleKey})` : title}
      onClick={() => pick(v)}
      className={clsx(
        "relative flex size-11 items-center justify-center text-muted hover:text-fg",
        sidebar === v && "text-fg",
        sidebar === v && !sidebarCollapsed && "before:absolute before:left-0 before:h-6 before:w-0.5 before:rounded-r before:bg-accent",
      )}
    >
      {icon}
    </button>
  );
  return (
    <div className="glass-side flex w-11 shrink-0 flex-col items-center border-r border-line bg-app pt-1">
      {item("claude", <AiTerminalIcon className="size-5" />, "AI Workspace")}
      {item("connections", <Cable className="size-5" />, "Connections")}
      {item("history", <History className="size-5" />, "History & saved queries")}
      {item("network", <Shield className="size-5" />, "VPN & SSH tunnels")}
      {item("git", <GitBranch className="size-5" />, "Git repositories")}
      {item("notes", <NotebookPen className="size-5" />, "Notes")}
      <div className="flex-1" />
      <button
        title={`${sidebarCollapsed ? "Show" : "Hide"} sidebar (${toggleKey})`}
        onClick={() => setSidebarCollapsed(!sidebarCollapsed)}
        className="mb-1 flex size-11 items-center justify-center text-muted hover:text-fg"
      >
        {sidebarCollapsed ? <PanelLeftOpen className="size-4.5" /> : <PanelLeftClose className="size-4.5" />}
      </button>
    </div>
  );
}

function Sidebar() {
  const sidebar = useUi((s) => s.sidebar);
  const openDialog = useUi((s) => s.openConnectionDialog);
  return (
    <div className="flex h-full flex-col bg-panel">
      <div className="flex h-9 shrink-0 items-center justify-between border-b border-line px-3">
        <span className="text-[11px] font-semibold uppercase tracking-wider text-muted">
          {sidebar === "connections" ? "Connections" : sidebar === "network" ? "VPN & Tunnels" : sidebar === "claude" ? "AI Workspace" : sidebar === "git" ? "Git" : sidebar === "notes" ? "Notes" : "History"}
        </span>
        {sidebar === "connections" && (
          <IconButton title="New connection" onClick={() => openDialog(null)}><Plus className="size-4" /></IconButton>
        )}
        {sidebar === "claude" && <ProviderPicker />}
        {sidebar === "notes" && (
          <IconButton title="New note" onClick={() => void createNote()}><Plus className="size-4" /></IconButton>
        )}
      </div>
      <div className="min-h-0 flex-1 overflow-hidden">
        {sidebar === "connections" ? <ConnectionTree /> : sidebar === "network" ? <NetworkPanel /> : sidebar === "claude" ? <ClaudePanel /> : sidebar === "git" ? <GitPanel /> : sidebar === "notes" ? <NotesPanel /> : <HistoryPanel />}
      </div>
    </div>
  );
}

/** Top bar: project switcher; while a project is focused it shows which one and how to leave. */
function ProjectBar() {
  const projects = useConnections((s) => s.projects);
  const connections = useConnections((s) => s.connections);
  const setFocus = useConnections((s) => s.setFocus);
  const focus = useFocusProject();
  const menu = useContextMenu();

  // Native window title follows the focused project.
  useEffect(() => {
    getCurrentWindow().setTitle(focus ? `DevCat — ${focus.name}` : "DevCat").catch(() => {});
  }, [focus]);

  const count = focus ? connections.filter((c) => c.projectId === focus.id).length : 0;
  const openSwitcher = (e: React.MouseEvent) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const items: MenuItem[] = [
      { label: "All projects", icon: <Layers className="size-3.5" />, disabled: !focus, onClick: () => setFocus(null) },
      ...(projects.length ? ["separator" as const] : []),
      ...projects.map((p): MenuItem => ({
        label: p.name,
        icon: <Folder className="size-3.5" style={{ color: p.color ?? undefined }} />,
        disabled: focus?.id === p.id,
        onClick: () => focusProject(p.id),
      })),
      "separator",
      { label: "New project…", icon: <FolderPlus className="size-3.5" />, onClick: async () => { const p = await createProject(); if (p) focusProject(p.id); } },
      "separator",
      { label: "New window", icon: <AppWindow className="size-3.5" />, shortcut: NEW_WINDOW_SHORTCUT, onClick: () => void openNewWindow() },
      { label: "Check for updates…", icon: <ArrowUpCircle className="size-3.5" />, onClick: () => void useUpdate.getState().check(true) },
      ...(projects.length
        ? [{
            label: "Open project in new window",
            icon: <SquareArrowOutUpRight className="size-3.5" />,
            submenu: projects.map((p): MenuItem => ({
              label: p.name,
              icon: <Folder className="size-3.5" style={{ color: p.color ?? undefined }} />,
              onClick: () => void openNewWindow(p.id),
            })),
          } as MenuItem]
        : []),
    ];
    menu.show({ clientX: r.left, clientY: r.bottom + 2 }, items);
  };

  return (
    <div
      className={clsx("glass-side flex h-8 shrink-0 items-center gap-2 border-b px-2 text-[12px]", focus ? "border-b-2" : "border-line bg-app")}
      style={focus ? { borderBottomColor: focus.color ?? "var(--color-accent)", background: `color-mix(in srgb, ${focus.color ?? "var(--color-accent)"} 12%, var(--color-app))` } : undefined}
    >
      <button
        onClick={openSwitcher}
        title="Switch project focus"
        className="flex h-6 max-w-72 items-center gap-1.5 rounded px-2 hover:bg-hover"
      >
        {focus ? <Crosshair className="size-3.5 shrink-0" style={{ color: focus.color ?? "var(--color-accent)" }} /> : <Layers className="size-3.5 shrink-0 text-muted" />}
        <span className={clsx("truncate", focus ? "font-semibold text-fg" : "text-muted")}>{focus ? focus.name : "All projects"}</span>
        <ChevronDown className="size-3 shrink-0 text-muted" />
      </button>
      {focus && (
        <>
          <span className="rounded bg-black/25 px-1.5 py-px text-[10px] font-semibold tracking-wider text-fg/80 uppercase">Focus mode</span>
          <span className="truncate text-muted">
            {count} connection{count === 1 ? "" : "s"}
          </span>
          <button onClick={() => setFocus(null)} className="ml-auto flex h-6 items-center gap-1.5 rounded px-2 text-muted hover:bg-hover hover:text-fg">
            <LogOut className="size-3.5" />Exit focus
          </button>
        </>
      )}
      <div className={clsx("flex items-center gap-0.5", !focus && "ml-auto")}>
        <AppearanceButton />
        <MemoryButton />
        <RoomsButton />
        <TicketsButton />
        <MobileButton />
        <KeepAwakeButton />
        <IconButton title={`New window (${NEW_WINDOW_SHORTCUT})`} onClick={() => void openNewWindow(focus?.id)}>
          <AppWindow className="size-3.5" />
        </IconButton>
      </div>
      {menu.element}
    </div>
  );
}

/** AI workspace tab title: shimmering "working" while the agent runs, a dot when it finished unseen. */
function AgentTabTitle({ tabId, title }: { tabId: string; title: string }) {
  const { working, done } = useTabActivity(tabId);
  return (
    <>
      {working && <Sparkle className="size-3 shrink-0 animate-spin text-accent [animation-duration:2.4s]" />}
      <span className="flex min-w-0 flex-col leading-tight">
        <span className="truncate">{title}</span>
        {working && <span className="shimmer-text text-[10px] font-medium">working</span>}
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

function Workspace() {
  const { tabs: allTabs, activeId, activate, close, closeOthers } = useTabs();
  const menu = useContextMenu();
  const tabMenu = (e: React.MouseEvent, t: Tab) => {
    const focusId = useConnections.getState().focusProjectId;
    const transferable = t.type !== "welcome";
    menu.show(e, [
      { label: "Move to new window", icon: <SquareArrowOutUpRight className="size-3.5" />, disabled: !transferable, onClick: () => void moveTabToWindow(t) },
      // A copy of an AI workspace tab would run a second agent on the same session.
      ...(t.type !== "claude"
        ? [{ label: "Open copy in new window", icon: <Copy className="size-3.5" />, disabled: !transferable, onClick: async () => void openNewWindow(focusId, await tabTransfer(t)) } as MenuItem]
        : []),
      // Another shell / file browser on the same host (Xshell's duplicate session).
      ...(t.type === "terminal" || t.type === "sftp"
        ? [{ label: "Duplicate tab", icon: <CopyPlus className="size-3.5" />, onClick: () => { const { id: _id, title, dirty: _d, ...spec } = t; openTab(spec, title); } } as MenuItem]
        : []),
      ...((t.type === "terminal" || t.type === "claude") && !useQuickCommands.getState().visible
        ? [{ label: "Show quick command bar", icon: <Zap className="size-3.5" />, onClick: () => useQuickCommands.getState().setVisible(true) } as MenuItem]
        : []),
      "separator",
      { label: "Close", icon: <X className="size-3.5" />, onClick: () => close(t.id) },
      { label: "Close others", disabled: allTabs.length < 2, onClick: () => closeOthers(t.id) },
    ]);
  };
  // Focus mode hides tabs of other projects' connections and notes (they stay mounted and come back on exit).
  const focusId = useConnections((s) => s.focusProjectId);
  const connections = useConnections((s) => s.connections);
  const notes = useNotes((s) => s.notes);
  const tabs = useMemo(
    () => allTabs.filter((t) => t.type === "note"
      ? noteInFocus(notes.find((n) => n.id === t.noteId)?.projectId)
      : inFocus(tabConnId(t))),
    [allTabs, focusId, connections, notes], // eslint-disable-line react-hooks/exhaustive-deps
  );
  const hiddenCount = allTabs.length - tabs.length;
  const connName = (t: Tab) => { const id = tabConnId(t); return id ? connections.find((c) => c.id === id)?.name : undefined; };
  const connColor = (t: Tab) => { const id = tabConnId(t); return id ? connections.find((c) => c.id === id)?.color : undefined; };
  useEffect(() => {
    if (!tabs.some((t) => t.id === activeId) && tabs.length) activate(tabs[tabs.length - 1].id);
  }, [tabs, activeId, activate]);
  const { barRef, drag, startDrag } = useTabDrag();
  return (
    <div className="flex h-full flex-col bg-panel">
      <div ref={barRef} className="no-select flex h-9 shrink-0 items-stretch overflow-x-auto border-b border-line bg-app">
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
              t.id === activeId ? "bg-panel text-fg shadow-[inset_0_2px_0_var(--color-accent)]" : "text-muted hover:bg-panel/60",
              drag?.id === t.id && "opacity-50",
            )}
          >
            {/* Where the dragged tab lands: before this one (or after the last). */}
            {drag && drag.before === t.id && drag.id !== t.id && tabs[i - 1]?.id !== drag.id && (
              <span className="pointer-events-none absolute inset-y-1 -left-px w-0.5 rounded bg-accent" />
            )}
            {drag && drag.before === null && i === tabs.length - 1 && drag.id !== t.id && (
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
        {hiddenCount > 0 && (
          <span className="ml-auto flex shrink-0 items-center px-3 text-[11px] text-faint" title="Tabs from other projects are hidden while focus mode is on">
            {hiddenCount} tab{hiddenCount === 1 ? "" : "s"} hidden by focus
          </span>
        )}
      </div>
      {menu.element}
      <div className="relative min-h-0 flex-1">
        {/* Keep every tab mounted so editors and grids preserve their state. */}
        {allTabs.map((t) => (
          <div key={t.id} className={clsx("absolute inset-0", (t.id !== activeId || !tabs.includes(t)) && "hidden")}>
            <TabContent tab={t} />
          </div>
        ))}
      </div>
    </div>
  );
}

/** Reorder tabs by dragging them along the tab bar (mouse events: Tauri's file drop handling
 *  gets in the way of HTML drag and drop). The bar scrolls when dragging near its edges. */
function useTabDrag() {
  const barRef = useRef<HTMLDivElement>(null);
  const [drag, setDrag] = useState<{ id: string; before: string | null } | null>(null);
  const startDrag = (e: React.MouseEvent, id: string) => {
    const x0 = e.clientX;
    let moving = false;
    // The tab whose left half is under the pointer, or the next one; null = past the last.
    const target = (x: number): string | null => {
      for (const el of barRef.current?.querySelectorAll<HTMLElement>("[data-tab-id]") ?? []) {
        const r = el.getBoundingClientRect();
        if (x < r.left + r.width / 2) return el.dataset.tabId ?? null;
      }
      return null;
    };
    const onMove = (ev: MouseEvent) => {
      if (!moving && Math.abs(ev.clientX - x0) < 5) return;
      moving = true;
      const bar = barRef.current;
      if (bar) {
        const r = bar.getBoundingClientRect();
        if (ev.clientX < r.left + 24) bar.scrollLeft -= 16;
        else if (ev.clientX > r.right - 24) bar.scrollLeft += 16;
      }
      const before = target(ev.clientX);
      setDrag((d) => (d?.before === before && d.id === id ? d : { id, before }));
    };
    const onUp = (ev: MouseEvent) => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      if (moving) useTabs.getState().move(id, target(ev.clientX));
      setDrag(null);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };
  return { barRef, drag, startDrag };
}

function StatusBar() {
  const status = useUi((s) => s.status);
  const open = useConnections((s) => s.open);
  const update = useUpdate((s) => (s.phase === "available" || s.phase === "error" ? s.info : null));
  const checking = useUpdate((s) => s.phase === "checking");
  const [version, setVersion] = useState<string | null>(null);
  useEffect(() => {
    getVersion().then(setVersion).catch(() => {});
    // One window checks for updates in the background.
    if (IS_MAIN_WINDOW) startUpdateChecks();
  }, []);
  return (
    <div className="glass-side flex h-6 shrink-0 items-center justify-between gap-3 border-t border-line bg-app px-3 text-[11px] text-muted">
      <span className="min-w-0 flex-1 truncate">{status}</span>
      {update && (
        <button className="flex items-center gap-1 rounded px-1.5 text-accent hover:bg-hover" onClick={() => useUpdate.getState().show()} title="Show the update">
          <ArrowUpCircle className="size-3" />Update to {update.version}
        </button>
      )}
      <span>{open.size} open connection{open.size === 1 ? "" : "s"}</span>
      {version && (
        <button className="text-faint hover:text-muted" onClick={() => void useUpdate.getState().check(true)} title="Check for updates">
          {checking ? "Checking…" : `v${version}`}
        </button>
      )}
    </div>
  );
}
