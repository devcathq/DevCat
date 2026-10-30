// The tour of the window: one part at a time, highlighted, with what it's for. Back / Next, Skip
// (or Esc) at any step. Each step points at an element with `data-tour="…"`; a step whose element
// isn't on screen shows its card in the middle instead.
import { useCallback, useEffect, useLayoutEffect, useState } from "react";
import { Button } from "../../components/ui";
import { IS_MAC } from "../../lib/windows";
import { useUi } from "../../store/ui";
import type { SidebarView } from "../../store/ui";
import { endTour, useOnboarding } from "./store";

interface Step {
  target: string;
  title: string;
  body: string;
  /** Show this sidebar first (so the step's element is there). */
  sidebar?: SidebarView;
}

const STEPS: Step[] = [
  { target: "projects", title: "Projects", body: "Group connections, chats and repos by project, and focus on one at a time: the others' tabs step aside." },
  { target: "sidebar-claude", sidebar: "claude", title: "AI workspace", body: "Your coding agent in a project folder: Claude Code, Codex, Gemini or the built-in DevCat Agent, with the folder's files, sessions and diff." },
  { target: "new-chat", sidebar: "claude", title: "Start a chat", body: "Pick a folder, then start a chat. The agent asks before it edits files or runs commands, and you review what it changed in the Diff tab." },
  { target: "sidebar-connections", sidebar: "connections", title: "Connections", body: "PostgreSQL, MySQL, SQL Server, SQLite, MongoDB, Redis, Kubernetes, Docker, RabbitMQ and SSH. Right-click one → Ask DevCat Agent for a chat that already knows it." },
  { target: "sidebar-network", sidebar: "network", title: "VPN & tunnels", body: "OpenVPN, WireGuard, SSH tunnels and Kubernetes port-forwards. A connection can depend on one: it's connected first, by itself." },
  { target: "sidebar-git", sidebar: "git", title: "Git", body: "Every repository in a folder, with the commit graph, blame, branches and stashes." },
  { target: "sidebar-notes", sidebar: "notes", title: "Notes", body: "Markdown notes per project, saved as you type. Agents can read and save them too." },
  { target: "sidebar-tickets", sidebar: "tickets", title: "Tickets", body: "Point the agent at a Jira, Trello or Linear column: it fixes each ticket on its own branch and moves it on." },
  { target: "sidebar-rooms", sidebar: "rooms", title: "Rooms", body: "Work with other people, peer to peer and end-to-end encrypted: files, docs, whiteboards, a thread, tasks, and everyone's agents." },
  { target: "tabs", sidebar: "claude", title: "Tabs", body: `Everything opens in a tab: drag to reorder, or move one to its own window (${IS_MAC ? "⇧⌘N" : "Ctrl+Shift+N"} opens a new window).` },
  { target: "ghost", title: "Ghost chat", body: "A fresh DevCat Agent chat in its own window: no project, no history, no memory. Close it and it's gone." },
  { target: "mobile", title: "Mobile pairing", body: "Follow and answer your agents from your phone, with DevCat Mobile or the phone's browser. No account, no server." },
  { target: "awake", title: "Keep awake", body: "Keeps the computer from sleeping while agents work; on macOS even with the lid closed." },
  { target: "settings", title: "Settings", body: "Appearance, the memory every agent shares, agent bots for Telegram and Discord, and updates. The tour and the setup guide are here too." },
];

const PAD = 6;
const CARD_W = 340;

export function Tour() {
  const open = useOnboarding((s) => s.tour);
  const [i, setI] = useState(0);
  const [rect, setRect] = useState<DOMRect | null>(null);
  const step = STEPS[i];

  useEffect(() => { if (open) setI(0); }, [open]);

  // show the step's sidebar, then find its element (after the sidebar has rendered)
  useLayoutEffect(() => {
    if (!open) return;
    if (step.sidebar) {
      const ui = useUi.getState();
      ui.setSidebar(step.sidebar);
      ui.setSidebarCollapsed(false);
    }
    let raf = requestAnimationFrame(() => {
      raf = requestAnimationFrame(() => {
        const el = document.querySelector(`[data-tour="${step.target}"]`);
        const r = el?.getBoundingClientRect();
        setRect(r && r.width > 0 && r.height > 0 ? r : null);
      });
    });
    return () => cancelAnimationFrame(raf);
  }, [open, step]);

  useEffect(() => {
    if (!open) return;
    const onResize = () => {
      const r = document.querySelector(`[data-tour="${step.target}"]`)?.getBoundingClientRect();
      setRect(r && r.width > 0 ? r : null);
    };
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, [open, step]);

  const next = useCallback(() => (i < STEPS.length - 1 ? setI(i + 1) : endTour()), [i]);
  const back = useCallback(() => setI(Math.max(0, i - 1)), [i]);

  useEffect(() => {
    if (!open) return;
    const h = (e: KeyboardEvent) => {
      if (e.key === "Escape") { e.preventDefault(); endTour(); }
      else if (e.key === "ArrowRight" || e.key === "Enter") { e.preventDefault(); next(); }
      else if (e.key === "ArrowLeft") { e.preventDefault(); back(); }
    };
    window.addEventListener("keydown", h, true);
    return () => window.removeEventListener("keydown", h, true);
  }, [open, next, back]);

  if (!open) return null;

  // the card: beside the element (right, else below, else left), kept on screen
  const vw = window.innerWidth, vh = window.innerHeight;
  let left = (vw - CARD_W) / 2, top = vh / 2 - 100;
  if (rect) {
    if (rect.right + PAD + 16 + CARD_W < vw) { left = rect.right + PAD + 16; top = rect.top; }
    else if (rect.bottom + PAD + 16 + 200 < vh) { left = rect.left; top = rect.bottom + PAD + 16; }
    else { left = rect.left - PAD - 16 - CARD_W; top = rect.top; }
    left = Math.max(12, Math.min(left, vw - CARD_W - 12));
    top = Math.max(12, Math.min(top, vh - 230));
  }

  return (
    <div className="fixed inset-0" style={{ zIndex: 70 }} role="dialog" aria-modal="true" aria-label={`Tour: ${step.title}`}>
      {/* catches clicks: the tour is a look around, not a click-through */}
      <div className="absolute inset-0" onMouseDown={(e) => e.preventDefault()} />
      {rect ? (
        <div className="pointer-events-none absolute rounded-lg ring-2 ring-accent transition-all duration-200"
          style={{ left: rect.left - PAD, top: rect.top - PAD, width: rect.width + PAD * 2, height: rect.height + PAD * 2, boxShadow: "0 0 0 9999px rgba(0,0,0,.62)" }} />
      ) : (
        <div className="pointer-events-none absolute inset-0 bg-black/60" />
      )}
      <div className="absolute rounded-xl border border-line bg-panel p-4 shadow-2xl" style={{ left, top, width: CARD_W }}>
        <div className="text-[11px] font-semibold tracking-wider text-accent uppercase">{i + 1} of {STEPS.length}</div>
        <div className="mt-1 text-[15px] font-semibold">{step.title}</div>
        <p className="mt-1.5 text-[13px] leading-relaxed text-muted">{step.body}</p>
        <div className="mt-3 flex items-center gap-2">
          <button className="text-[12px] text-faint hover:text-fg" onClick={endTour}>Skip tour</button>
          <div className="flex-1" />
          {i > 0 && <Button size="sm" variant="ghost" onClick={back}>Back</Button>}
          <Button size="sm" variant="primary" autoFocus onClick={next}>{i < STEPS.length - 1 ? "Next" : "Done"}</Button>
        </div>
        <div className="mt-3 flex gap-1">
          {STEPS.map((_, n) => <span key={n} className={`h-1 flex-1 rounded-full ${n <= i ? "bg-accent" : "bg-line"}`} />)}
        </div>
      </div>
    </div>
  );
}
