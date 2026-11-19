// Plan usage of the AI connections that log in (Claude Code's 5-hour and weekly limits, Antigravity's
// pools): `ai_usage` (src-tauri/src/ai/usage.rs). Read every few minutes while DevCat is open (the
// backend keeps results a few minutes too), shown in the title bar, the AI connections dialog and on phones.
import clsx from "clsx";
import { Gauge, RefreshCw } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { create } from "zustand";
import { Spin } from "../../components/ui";
import { api } from "../../lib/api";
import type { AiUsage, UsageWindow } from "../../lib/types";
import { CLAUDE_PROVIDER_ID, loadProviders, openManageProviders, providerLabel, useProviders } from "./providers";

const EVERY_MS = 5 * 60_000;

interface UsageState {
  byId: Record<string, AiUsage>;
  loading: boolean;
  at: number;
}

export const useUsage = create<UsageState>(() => ({ byId: {}, loading: false, at: 0 }));

let inflight: Promise<void> | null = null;
export function loadUsage(refresh = false): Promise<void> {
  if (inflight) return inflight;
  useUsage.setState({ loading: true });
  inflight = api.aiUsage(null, refresh)
    .then((list) => useUsage.setState({ byId: Object.fromEntries((list ?? []).map((u) => [u.providerId, u])), at: Date.now() }))
    .catch(() => { /* backend not ready: keep what's shown */ })
    .finally(() => { inflight = null; useUsage.setState({ loading: false }); });
  return inflight;
}

let polling = false;
/** Start reading usage (once per window): now, every few minutes, and on focus when it's old. */
export function watchUsage() {
  if (polling) return;
  polling = true;
  void loadUsage();
  setInterval(() => { if (document.visibilityState === "visible") void loadUsage(); }, EVERY_MS);
  window.addEventListener("focus", () => { if (Date.now() - useUsage.getState().at > EVERY_MS / 2) void loadUsage(); });
}

export const usageTone = (p: number) => (p >= 90 ? "danger" : p >= 75 ? "warn" : "ok");
const toneText = { ok: "text-muted", warn: "text-warn", danger: "text-danger" } as const;
const toneBar = { ok: "bg-accent", warn: "bg-warn", danger: "bg-danger" } as const;

/** "in 2h 10m", "in 3d" */
export function resetsIn(iso?: string | null): string | null {
  if (!iso) return null;
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return null;
  const mins = Math.max(0, Math.round((t - Date.now()) / 60_000));
  if (mins < 60) return `in ${mins}m`;
  if (mins < 48 * 60) return `in ${Math.floor(mins / 60)}h ${String(mins % 60).padStart(2, "0")}m`;
  return `in ${Math.round(mins / 1440)}d`;
}

function Bar({ w }: { w: UsageWindow }) {
  const tone = usageTone(w.percent);
  const reset = resetsIn(w.resetsAt);
  return (
    <div className="flex flex-col gap-0.5">
      <div className="flex items-baseline justify-between gap-3 text-[11px]">
        <span className="truncate text-muted">{w.label}</span>
        <span className={clsx("shrink-0 tabular-nums", toneText[tone])}>{Math.round(w.percent)}%{reset && <span className="text-faint"> · resets {reset}</span>}</span>
      </div>
      <div className="h-1 overflow-hidden rounded bg-hover">
        <div className={clsx("h-full rounded", toneBar[tone])} style={{ width: `${Math.min(100, Math.max(0, w.percent))}%` }} />
      </div>
    </div>
  );
}

/** One connection's limits (the AI connections dialog, the title bar's panel). */
export function UsageBars({ usage }: { usage?: AiUsage | null }) {
  if (!usage) return null;
  if (usage.error) return <span className="text-[11px] text-faint">{usage.error}</span>;
  if (!usage.windows.length) return <span className="text-[11px] text-faint">No limits reported.</span>;
  return <div className="flex flex-col gap-1.5">{usage.windows.map((w) => <Bar key={w.label} w={w} />)}</div>;
}

/** The usage section of a connection in the AI connections dialog. */
export function ConnectionUsage({ providerId }: { providerId: string }) {
  const usage = useUsage((s) => s.byId[providerId]);
  const loading = useUsage((s) => s.loading);
  useEffect(() => { watchUsage(); }, []);
  if (!usage) return null;
  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex items-center gap-2 text-xs text-muted">
        <span>Plan usage{usage.plan ? ` (${usage.plan})` : ""}</span>
        <button title="Read again" onClick={() => void loadUsage(true)} className="text-faint hover:text-fg"><Spin icon={RefreshCw} spinning={loading} className="size-3" /></button>
      </div>
      <UsageBars usage={usage} />
    </div>
  );
}

/** Title-bar chip: the most used limit of the connection new chats use (else of any connection
 *  with numbers); click for every connection's limits. Hidden while nothing has numbers. */
export function UsageChip() {
  const byId = useUsage((s) => s.byId);
  const loading = useUsage((s) => s.loading);
  const providers = useProviders((s) => s.providers);
  const selectedId = useProviders((s) => s.selectedId);
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => { watchUsage(); void loadProviders(); }, []);
  useEffect(() => {
    if (!open) return;
    const close = (e: PointerEvent) => { if (!ref.current?.contains(e.target as Node)) setOpen(false); };
    const esc = (e: KeyboardEvent) => { if (e.key === "Escape") setOpen(false); };
    window.addEventListener("pointerdown", close);
    window.addEventListener("keydown", esc);
    return () => { window.removeEventListener("pointerdown", close); window.removeEventListener("keydown", esc); };
  }, [open]);

  const all = Object.values(byId);
  const withNumbers = all.filter((u) => u.percent != null);
  const main = [byId[selectedId], byId[CLAUDE_PROVIDER_ID], ...withNumbers].find((u) => u && u.percent != null);
  if (!main) return null;
  const p = Math.round(main.percent ?? 0);
  const name = (id: string) => { const pr = providers.find((x) => x.id === id); return pr ? providerLabel(pr) : id; };
  const top = main.windows.reduce((a, b) => (b.percent > a.percent ? b : a), main.windows[0]);
  return (
    <div ref={ref} className="relative">
      <button
        onClick={() => setOpen((o) => !o)}
        title={`${name(main.providerId)}: ${top?.label ?? "usage"} ${p}% used. Click for every limit.`}
        aria-label="Plan usage"
        className={clsx("flex h-6 items-center gap-1 rounded px-1.5 text-[11px] tabular-nums hover:bg-hover", toneText[usageTone(p)])}
      >
        <Gauge className="size-3.5" />{p}%
      </button>
      {open && (
        <div className="absolute top-7 right-0 z-50 flex w-80 flex-col gap-3 rounded-md border border-line bg-raised p-3 shadow-lg">
          <div className="flex items-center justify-between text-xs">
            <span className="font-medium text-fg">Plan usage</span>
            <button title="Read again" onClick={() => void loadUsage(true)} className="text-faint hover:text-fg"><Spin icon={RefreshCw} spinning={loading} className="size-3.5" /></button>
          </div>
          {all.map((u) => (
            <div key={u.providerId} className="flex flex-col gap-1.5">
              <div className="truncate text-[12px] text-fg">{name(u.providerId)}{u.plan ? <span className="text-faint"> · {u.plan}</span> : null}</div>
              <UsageBars usage={u} />
            </div>
          ))}
          <button className="self-start text-[11px] text-accent hover:underline" onClick={() => { setOpen(false); openManageProviders(main.providerId); }}>Manage AI connections…</button>
        </div>
      )}
    </div>
  );
}
