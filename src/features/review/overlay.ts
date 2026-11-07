// The review's page is a native webview drawn above the window's HTML, so a DevCat menu, dialog,
// the command palette or a toast over the page area would be hidden behind it. While one covers
// the page area, the tab hides the page (see ReviewTab).
import { useEffect, useState, type RefObject } from "react";

type Rect = Pick<DOMRect, "left" | "top" | "right" | "bottom" | "width" | "height">;

/** Whether any overlay rect overlaps the page rect (touching an edge doesn't; 0-size rects never do). */
export function coveringRect(page: Rect, overlays: Rect[]): boolean {
  if (page.width <= 0 || page.height <= 0) return false;
  return overlays.some((o) =>
    o.width > 0 && o.height > 0 &&
    o.left < page.right && o.right > page.left && o.top < page.bottom && o.bottom > page.top);
}

/** DevCat's overlays are `fixed` (Tailwind's class: dialogs, ContextMenu, QuickOpen, the tour, popovers)
 *  or sonner toasts (absolute inside a fixed list: their own rects). */
const CANDIDATES = '.fixed, [style*="fixed"], [data-sonner-toast]';

function overlayRects(page: HTMLElement): DOMRect[] {
  const out: DOMRect[] = [];
  for (const el of document.body.querySelectorAll<HTMLElement>(CANDIDATES)) {
    if (page.contains(el)) continue;
    if (!el.hasAttribute("data-sonner-toast") && getComputedStyle(el).position !== "fixed") continue;
    out.push(el.getBoundingClientRect());
  }
  return out;
}

/** True while a DevCat overlay covers `ref`'s element. Watches only while `active` (the tab shows). */
export function useOverlayCovers(ref: RefObject<HTMLElement | null>, active = true): boolean {
  const [covers, setCovers] = useState(false);
  useEffect(() => {
    if (!active) { setCovers(false); return; }
    let frame = 0;
    const check = () => {
      frame = 0;
      const el = ref.current;
      setCovers(!!el && coveringRect(el.getBoundingClientRect(), overlayRects(el)));
    };
    // one check per frame, however many changes came in it
    const schedule = () => { if (!frame) frame = requestAnimationFrame(check); };
    const mo = new MutationObserver(schedule);
    mo.observe(document.body, { childList: true, subtree: true });
    window.addEventListener("resize", schedule);
    schedule();
    return () => {
      mo.disconnect();
      window.removeEventListener("resize", schedule);
      if (frame) cancelAnimationFrame(frame);
    };
  }, [ref, active]);
  return covers;
}
