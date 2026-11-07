// "Review page…": asks for the page's address and opens a Review tab. Opened from the Notes panel
// and the menus that offer new windows (AppShell, SettingsMenu); mounted once in AppShell.
import { create } from "zustand";
import { Button, Dialog } from "../../components/ui";
import { useConnections } from "../../store/connections";
import { openReview } from "./store";

const DEFAULT_REVIEW_URL = "http://localhost:5173";
const lastUrlKey = (projectId: string | null) => `devcat.review.lastUrl.${projectId ?? "general"}`;
function readLastUrl(projectId: string | null): string {
  try { return localStorage.getItem(lastUrlKey(projectId)) || DEFAULT_REVIEW_URL; } catch { return DEFAULT_REVIEW_URL; }
}

const useReviewDialog = create<{ open: boolean; url: string }>(() => ({ open: false, url: DEFAULT_REVIEW_URL }));

/** Opens the dialog with the last address reviewed in the focused project. */
export function openReviewDialog(): void {
  useReviewDialog.setState({ open: true, url: readLastUrl(useConnections.getState().focusProjectId ?? null) });
}

export function ReviewUrlDialog() {
  const { open, url } = useReviewDialog();
  const close = () => useReviewDialog.setState({ open: false });
  const start = () => {
    const u = url.trim();
    if (!u) return;
    try { localStorage.setItem(lastUrlKey(useConnections.getState().focusProjectId ?? null), u); } catch { /* remembering is optional */ }
    close();
    openReview({ url: u });
  };
  return (
    <Dialog
      open={open}
      onClose={close}
      title="Review a page"
      width={420}
      footer={<Button variant="primary" onClick={start} disabled={!url.trim()}>Review</Button>}
    >
      <label className="flex flex-col gap-1 text-xs text-muted">
        Page address
        <input
          autoFocus
          value={url}
          onChange={(e) => useReviewDialog.setState({ url: e.target.value })}
          onKeyDown={(e) => { if (e.key === "Enter" && !e.nativeEvent.isComposing) start(); }}
          spellCheck={false}
          className="h-8 rounded border border-line bg-app px-2 font-mono text-xs text-fg focus:border-accent focus:outline-none"
        />
      </label>
    </Dialog>
  );
}
