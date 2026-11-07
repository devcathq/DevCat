// Opening a Review tab: a web page (or a local .html file) shown in a native webview over the tab,
// commented element by element and saved as one note (see ReviewTab).
import { openTab } from "../../store/tabs";

export type ReviewTarget = { url: string } | { path: string };

/** `Review · localhost:5173` / `Review · index.html` (the review's own title replaces it once started). */
export function reviewTitle(target: ReviewTarget): string {
  if ("path" in target) return `Review · ${target.path.split(/[\\/]/).pop() || target.path}`;
  const raw = target.url.trim();
  try {
    return `Review · ${new URL(raw.includes("://") ? raw : `http://${raw}`).host}`;
  } catch {
    return `Review · ${raw}`;
  }
}

export function openReview(target: ReviewTarget): void {
  openTab("path" in target ? { type: "review", path: target.path } : { type: "review", url: target.url }, reviewTitle(target));
}
