// Title bar: the phone (pairing) and keep-awake icons, then one Settings menu (appearance, AI memory,
// new window, updates). The phone shows when it's on or a phone wants to pair; the cup, how long
// the computer is kept awake.
// Tickets have their own view in the left bar.
import clsx from "clsx";
import { AppWindow, ArrowUpCircle, Bot, Brain, Compass, Palette, Settings, Smartphone, Wand2 } from "lucide-react";
import { openSetup, startTour } from "../../features/onboarding/store";
import { openIntegrations } from "../../features/integrations/store";
import { openAppearance } from "../../features/appearance/AppearanceDialog";
import { useContextMenu, type MenuItem } from "../../features/connections/ContextMenu";
import { KeepAwakeButton, useKeepAwake } from "../../features/keepawake/KeepAwakeButton";
import { openMemory } from "../../features/memory/MemoryDialog";
import { useMobile } from "../../features/mobile/store";
import { useUpdate } from "../../features/update/store";
import { NEW_WINDOW_SHORTCUT, openNewWindow } from "../../lib/windows";
import { useConnections } from "../../store/connections";

export function SettingsMenu() {
  const menu = useContextMenu();
  const keepAwake = useKeepAwake();
  const mobile = useMobile((s) => s.status);
  const phoneOn = !!(mobile?.settings.enabled && mobile.running);
  const pairing = (mobile?.pending.length ?? 0) > 0;
  const open = (e: React.MouseEvent) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const items: MenuItem[] = [
      { label: "Appearance…", icon: <Palette className="size-3.5" />, onClick: openAppearance },
      { label: "AI memory…", icon: <Brain className="size-3.5" />, onClick: () => openMemory() },
      { label: "Agent bots…", icon: <Bot className="size-3.5" />, onClick: openIntegrations },
      "separator",
      { label: "New window", icon: <AppWindow className="size-3.5" />, shortcut: NEW_WINDOW_SHORTCUT, onClick: () => void openNewWindow(useConnections.getState().focusProjectId) },
      { label: "Check for updates…", icon: <ArrowUpCircle className="size-3.5" />, onClick: () => void useUpdate.getState().check(true) },
      "separator",
      { label: "Set up DevCat…", icon: <Wand2 className="size-3.5" />, onClick: openSetup },
      { label: "Take the tour", icon: <Compass className="size-3.5" />, onClick: startTour },
    ];
    // anchored under the gear, right-aligned (the menu keeps itself inside the window)
    menu.show({ clientX: r.right, clientY: r.bottom + 2 }, items);
  };

  return (
    <>
      <button
        onClick={() => useMobile.getState().show()}
        title={pairing ? "Mobile pairing: a phone wants to pair" : phoneOn ? "Mobile pairing (phone access is on)" : "Mobile pairing"}
        aria-label="Mobile pairing"
        data-tour="mobile"
        className={clsx("relative flex h-6 items-center rounded px-1.5 hover:bg-hover", phoneOn ? "text-accent" : "text-muted hover:text-fg")}
      >
        <Smartphone className="size-3.5" />
        {pairing && <span className="absolute top-0.5 right-0.5 size-1.5 rounded-full bg-warn" />}
      </button>
      <KeepAwakeButton keepAwake={keepAwake} />
      <button
        onClick={open}
        title="Settings"
        aria-label="Settings"
        data-tour="settings"
        className="flex h-6 items-center rounded px-1.5 text-muted hover:bg-hover hover:text-fg"
      >
        <Settings className="size-3.5" />
      </button>
      {menu.element}
    </>
  );
}
