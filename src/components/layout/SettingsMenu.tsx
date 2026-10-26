// Title bar: one Settings menu (appearance, AI memory, phone pairing, tickets, keep awake, new window)
// instead of a row of icons. A dot on the gear shows what needs a look; keep awake shows its own chip while on.
import clsx from "clsx";
import { AppWindow, ArrowUpCircle, Brain, Coffee, ListTodo, Palette, Settings, Smartphone } from "lucide-react";
import { useEffect } from "react";
import { openAppearance } from "../../features/appearance/AppearanceDialog";
import { useContextMenu, type MenuItem } from "../../features/connections/ContextMenu";
import { KeepAwakeButton, useKeepAwake } from "../../features/keepawake/KeepAwakeButton";
import { openMemory } from "../../features/memory/MemoryDialog";
import { useMobile } from "../../features/mobile/store";
import { openTickets, startTickets, useTickets } from "../../features/tickets/store";
import { useUpdate } from "../../features/update/store";
import { NEW_WINDOW_SHORTCUT, openNewWindow } from "../../lib/windows";
import { useConnections } from "../../store/connections";

export function SettingsMenu() {
  const menu = useContextMenu();
  const keepAwake = useKeepAwake();
  const ticketsWorking = useTickets((s) => Object.keys(s.jobs).length > 0);
  const mobile = useMobile((s) => s.status);
  const phoneOn = !!(mobile?.settings.enabled && mobile.running);
  const pairing = (mobile?.pending.length ?? 0) > 0;
  useEffect(() => {
    startTickets();
    void useTickets.getState().refresh().catch(() => {});
  }, []);

  const open = (e: React.MouseEvent) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const items: MenuItem[] = [
      { label: "Appearance…", icon: <Palette className="size-3.5" />, onClick: openAppearance },
      { label: "AI memory…", icon: <Brain className="size-3.5" />, onClick: () => openMemory() },
      {
        label: "Mobile pairing…",
        shortcut: pairing ? "pair?" : undefined,
        icon: <Smartphone className={clsx("size-3.5", pairing ? "text-warn" : phoneOn && "text-accent")} />,
        onClick: () => useMobile.getState().show(),
      },
      "separator",
      {
        label: "Tickets",
        icon: <ListTodo className={clsx("size-3.5", ticketsWorking && "text-accent")} />,
        shortcut: ticketsWorking ? "working" : undefined,
        onClick: openTickets,
      },
      { label: keepAwake.on ? `Keep awake (on${keepAwake.left ? `, ${keepAwake.left} left` : ""})` : "Keep awake", icon: <Coffee className={clsx("size-3.5", keepAwake.on && "text-accent")} />, submenu: keepAwake.items },
      "separator",
      { label: "New window", icon: <AppWindow className="size-3.5" />, shortcut: NEW_WINDOW_SHORTCUT, onClick: () => void openNewWindow(useConnections.getState().focusProjectId) },
      { label: "Check for updates…", icon: <ArrowUpCircle className="size-3.5" />, onClick: () => void useUpdate.getState().check(true) },
    ];
    // anchored under the gear, right-aligned (the menu keeps itself inside the window)
    menu.show({ clientX: r.right, clientY: r.bottom + 2 }, items);
  };

  const dot = pairing ? "bg-warn" : ticketsWorking ? "bg-accent animate-pulse" : null;
  return (
    <>
      <KeepAwakeButton keepAwake={keepAwake} />
      <button
        onClick={open}
        title={pairing ? "Settings: a phone wants to pair" : ticketsWorking ? "Settings: the agent is working on a ticket" : "Settings"}
        aria-label="Settings"
        className="relative flex h-6 items-center rounded px-1.5 text-muted hover:bg-hover hover:text-fg"
      >
        <Settings className="size-3.5" />
        {dot && <span className={clsx("absolute top-0.5 right-0.5 size-1.5 rounded-full", dot)} />}
      </button>
      {menu.element}
    </>
  );
}
