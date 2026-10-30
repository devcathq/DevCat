// Keeps the screen and system awake (like Caffeine / Amphetamine): the cup in the title bar turns
// it on (for a while or until turned off), and shows how long is left while it's on.
import { listen } from "@tauri-apps/api/event";
import { Coffee, Laptop, Power, Timer } from "lucide-react";
import { useEffect, useState, type MouseEvent } from "react";
import { toast } from "sonner";
import { api, errMsg } from "../../lib/api";
import type { KeepAwakeState } from "../../lib/types";
import { useContextMenu, type MenuItem } from "../connections/ContextMenu";
import { confirmDialog } from "../connections/dialogs";
import { IS_MAC } from "../../lib/windows";

const DURATIONS: [number, string][] = [[15, "15 minutes"], [60, "1 hour"], [120, "2 hours"], [300, "5 hours"], [480, "8 hours"]];

function remaining(until: number, now: number): string {
  const mins = Math.max(0, Math.ceil((until - now) / 60_000));
  return mins >= 60 ? `${Math.floor(mins / 60)}h ${String(mins % 60).padStart(2, "0")}m` : `${mins}m`;
}

/** Keep-awake state, shared by all windows: read once, then follow the backend's broadcasts. */
export function useKeepAwake() {
  const [state, setState] = useState<KeepAwakeState>({ on: false });
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    api.keepAwakeStatus().then((s) => { if (s) setState(s); }).catch(() => {});
    const un = listen<KeepAwakeState>("keep-awake", (e) => setState(e.payload));
    return () => { void un.then((f) => f()); };
  }, []);
  useEffect(() => {
    if (!state.until) return;
    const t = setInterval(() => setNow(Date.now()), 15_000);
    setNow(Date.now());
    return () => clearInterval(t);
  }, [state.until]);

  const set = async (on: boolean, minutes?: number, lid?: boolean) => {
    try {
      const s = await api.keepAwakeSet(on, minutes ?? null, lid ?? null);
      if (s) setState(s);
    } catch (e) {
      toast.error(errMsg(e));
    }
  };
  const items: MenuItem[] = [
    ...DURATIONS.map(([m, label]): MenuItem => ({ label: `Keep awake for ${label}`, icon: <Timer className="size-3.5" />, onClick: () => void set(true, m) })),
    { label: "Keep awake until turned off", icon: <Coffee className="size-3.5" />, onClick: () => void set(true) },
    // power assertions don't stop the sleep closing the lid forces (keep_awake.rs `lid`)
    ...(IS_MAC
      ? [state.lid
        ? { label: "Let it sleep when the lid closes", icon: <Laptop className="size-3.5" />, onClick: () => void set(true, undefined, false) } as MenuItem
        : { label: "Keep awake with the lid closed…", icon: <Laptop className="size-3.5" />, onClick: () => void lidClosed() } as MenuItem]
      : []),
    "separator",
    { label: "Allow sleep", icon: <Power className="size-3.5" />, disabled: !state.on, onClick: () => void set(false) },
  ];
  /** Until turned off, also with the lid closed: macOS sleep is turned off (with the password). */
  async function lidClosed() {
    const ok = await confirmDialog({
      title: "Keep awake with the lid closed?",
      message: "macOS sleeps when the lid closes, whatever apps ask. To stop that, DevCat turns system sleep off (pmset disablesleep), which asks for your password, and turns it back on when keep awake ends: when you turn it off, when DevCat quits, or at its next start if it crashed (that asks for the password again). Meanwhile your Mac doesn't sleep at all: don't leave it in a bag, where it can get hot, and keep it plugged in.",
      confirmLabel: "Keep awake with the lid closed",
    });
    if (ok) await set(true, undefined, true);
  }
  const left = state.on && state.until ? remaining(state.until, now) : null;
  return { on: state.on, lid: !!state.lid, left, set, items };
}

/** Title-bar cup. Off: click to keep awake for a while. On (a chip with the time left): click to
 *  allow sleep, right-click for a set time. */
export function KeepAwakeButton({ keepAwake }: { keepAwake: ReturnType<typeof useKeepAwake> }) {
  const { on, lid, left, set, items } = keepAwake;
  const menu = useContextMenu();
  const openMenu = (e: MouseEvent) => {
    e.preventDefault();
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    menu.show({ clientX: r.left, clientY: r.bottom + 2 }, items);
  };
  if (!on) {
    return (
      <>
        <button data-tour="awake" title="Keep awake: stop the screen and computer from sleeping" aria-label="Keep awake" onClick={openMenu} onContextMenu={openMenu}
          className="flex h-6 items-center rounded px-1.5 text-muted hover:bg-hover hover:text-fg">
          <Coffee className="size-3.5" />
        </button>
        {menu.element}
      </>
    );
  }
  return (
    <>
      <button
        title={`Keeping the screen awake${lid ? ", also with the lid closed" : ""}${left ? ` for ${left} more` : ""}. Click to allow sleep. Right-click for a set time.`}
        aria-pressed
        data-tour="awake"
        onClick={() => void set(false)}
        onContextMenu={openMenu}
        className="flex h-6 items-center gap-1 rounded bg-accent/20 px-1.5 text-[11px] text-accent tabular-nums hover:bg-accent/30"
      >
        <Coffee className="size-3.5" />
        {lid && <Laptop className="size-3.5" />}
        {left}
      </button>
      {menu.element}
    </>
  );
}
