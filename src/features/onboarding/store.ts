// First run: the setup guide (pick a coding agent, a folder, connections…) and the tour of the
// window. Both can be skipped, and opened again from Settings or the Welcome tab. Only the main
// window shows them on its own; a fresh install is one with no connections and no AI workspace
// folder yet, so people updating DevCat aren't stopped by the guide (they get a one-time offer of
// the tour instead).
import { toast } from "sonner";
import { create } from "zustand";
import { useWorkspace } from "../claude/sidebar/workspace";
import { useConnections } from "../../store/connections";
import { IS_MAIN_WINDOW } from "../../lib/windows";

const SETUP_DONE = "devcat.setup.done";
const TOUR_OFFERED = "devcat.tour.offered";

const flag = (k: string) => {
  try { return localStorage.getItem(k) === "1"; } catch { return true; }
};
const setFlag = (k: string) => {
  try { localStorage.setItem(k, "1"); } catch { /* private mode */ }
};

export const useOnboarding = create<{ setup: boolean; tour: boolean }>(() => ({ setup: false, tour: false }));

// (the "take the tour" notice, if it's still up, goes away)
export const openSetup = () => { toast.dismiss(); useOnboarding.setState({ setup: true, tour: false }); };
export const startTour = () => { toast.dismiss(); useOnboarding.setState({ setup: false, tour: true }); };
export const endTour = () => useOnboarding.setState({ tour: false });

/** The guide is done (finished or skipped): it doesn't open by itself again. */
export function finishSetup(thenTour = false) {
  setFlag(SETUP_DONE);
  setFlag(TOUR_OFFERED);
  useOnboarding.setState({ setup: false, tour: thenTour });
}

/** At startup (main window), once the connections are loaded: the guide for a fresh install, or a
 *  one-time offer of the tour for someone who already uses DevCat. */
export function firstRun() {
  if (!IS_MAIN_WINDOW || flag(SETUP_DONE)) return;
  const fresh = useConnections.getState().connections.length === 0 && !useWorkspace.getState().cwd;
  if (fresh) {
    openSetup();
    return;
  }
  setFlag(SETUP_DONE);
  if (flag(TOUR_OFFERED)) return;
  setFlag(TOUR_OFFERED);
  toast("New in DevCat: a quick tour of the window", {
    description: "Where the agents, connections, rooms and settings are. It takes a minute.",
    action: { label: "Take the tour", onClick: startTour },
    duration: 12000,
  });
}
