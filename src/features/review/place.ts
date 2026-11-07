// Where the Review tab asks Rust to put the native page (see ReviewTab's PageArea).

export interface Place { id: string; x: number; y: number; w: number; h: number; visible: boolean }

/**
 * The next place to ask for. `at` is where the page goes while it can show (`null`: hidden). Nothing
 * is created before the page can show, but once a place was sent (even one that failed) every hide
 * is sent, so the native page is never left over other UI. `null`: nothing to send.
 */
export function nextPlace(prev: Place | null, at: Omit<Place, "visible"> | null, everSent: boolean): Place | null {
  if (at) return { ...at, visible: true };
  if (!everSent || !prev) return null;
  return { ...prev, visible: false };
}
