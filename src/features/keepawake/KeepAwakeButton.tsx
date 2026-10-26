// Keeps the screen and system awake (like Caffeine / Amphetamine): the Settings menu turns it on,
// a title-bar chip shows it while it's on.
import { listen } from "@tauri-apps/api/event";
import { Coffee, Power, Timer } from "lucide-react";
import { useEffect, useState, type MouseEvent } from "react";
import { toast } from "sonner";
import { api, errMsg } from "../../lib/api";
import type { KeepAwakeState } from "../../lib/types";
import { useContextMenu, type MenuItem } from "../connections/ContextMenu";

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

  const set = async (on: boolean, minutes?: number) => {
    try {
      const s = await api.keepAwakeSet(on, minutes ?? null);
      if (s) setState(s);
    } catch (e) {
      toast.error(errMsg(e));
    }
  };
  const items: MenuItem[] = [
    ...DURATIONS.map(([m, label]): MenuItem => ({ label: `Keep awake for ${label}`, icon: <Timer className="size-3.5" />, onClick: () => void set(true, m) })),
    { label: "Keep awake until turned off", icon: <Coffee className="size-3.5" />, onClick: () => void set(true) },
    "separator",
    { label: "Allow sleep", icon: <Power className="size-3.5" />, disabled: !state.on, onClick: () => void set(false) },
  ];
  const left = state.on && state.until ? remaining(state.until, now) : null;
  return { on: state.on, left, set, items };
}

/** Title-bar chip while the screen is kept awake (turned on from the Settings menu).
 *  Click: allow sleep. Right-click: for a set time. */
export function KeepAwakeButton({ keepAwake }: { keepAwake: ReturnType<typeof useKeepAwake> }) {
  const { on, left, set, items } = keepAwake;
  const menu = useContextMenu();
  if (!on) return null;
  const openMenu = (e: MouseEvent) => {
    e.preventDefault();
    menu.show(e, items);
  };
  return (
    <>
      <button
        title={`Keeping the screen awake${left ? ` for ${left} more` : ""}. Click to allow sleep. Right-click for a set time.`}
        aria-pressed
        onClick={() => void set(false)}
        onContextMenu={openMenu}
        className="flex h-6 items-center gap-1 rounded bg-accent/20 px-1.5 text-[11px] text-accent tabular-nums hover:bg-accent/30"
      >
        <Coffee className="size-3.5" />
        {left}
      </button>
      {menu.element}
    </>
  );
}
