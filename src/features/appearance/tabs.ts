// Tab bar layout (per computer, localStorage; shared by all DevCat windows, like the fonts): one
// row that scrolls sideways, or rows that wrap (a tab that doesn't fit goes on a new row below).
import { create } from "zustand";

export type TabLayout = "scroll" | "wrap";
const KEY = "devcat.tabs";

function load(): TabLayout {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) ?? "{}")?.layout;
    return v === "wrap" ? "wrap" : "scroll";
  } catch {
    return "scroll";
  }
}

export const useTabLayout = create<{ layout: TabLayout; set: (layout: TabLayout) => void }>((set) => ({
  layout: load(),
  set: (layout) => {
    set({ layout });
    try { localStorage.setItem(KEY, JSON.stringify({ layout })); } catch { /* private mode */ }
  },
}));

window.addEventListener("storage", (e) => { if (e.key === KEY) useTabLayout.setState({ layout: load() }); });
