// Phone UI primitives in DevCat's dark theme: screen with header + bottom tabs, list items,
// cards, buttons, toggles, segmented controls, sheets and confirm dialogs.
import clsx from "clsx";
import { ChevronLeft, ChevronRight, FolderOpen, Loader2, MessageCircle, MoreHorizontal, TerminalSquare } from "lucide-react";
import { useEffect, useState, type ButtonHTMLAttributes, type InputHTMLAttributes, type ReactNode, type TextareaHTMLAttributes } from "react";
import { nav, useApp, waitingCount } from "./lib/store";
import type { TabRoot } from "./lib/types";

const TABS: { id: TabRoot; label: string; icon: ReactNode }[] = [
  { id: "agents", label: "Chats", icon: <MessageCircle className="size-[22px]" /> },
  { id: "terminals", label: "Terminal", icon: <TerminalSquare className="size-[22px]" /> },
  { id: "fileshome", label: "Files", icon: <FolderOpen className="size-[22px]" /> },
  { id: "more", label: "More", icon: <MoreHorizontal className="size-[22px]" /> },
];

export function TabBar({ active }: { active: TabRoot }) {
  const waiting = useApp(waitingCount);
  return (
    <nav className="flex shrink-0 justify-around border-t border-line bg-panel pt-1.5" style={{ paddingBottom: "max(10px, env(safe-area-inset-bottom))" }}>
      {TABS.map((t) => (
        <button key={t.id} onClick={() => nav.tab(t.id)} className={clsx("relative flex min-w-0 flex-1 flex-col items-center gap-0.5 py-0.5 text-[11px] font-medium", active === t.id ? "text-accent" : "text-faint")}>
          {t.icon}
          {t.label}
          {t.id === "agents" && waiting > 0 && <span className="absolute -top-0.5 right-[calc(50%-20px)] min-w-4 rounded-full bg-warn px-1 text-[10px] leading-4 font-bold text-accent-fg">{waiting}</span>}
        </button>
      ))}
    </nav>
  );
}

export function Screen({ title, sub, back: backProp, right, children, tab, scroll = true, className }: {
  title: ReactNode; sub?: ReactNode; back?: boolean; right?: ReactNode; children: ReactNode; tab?: TabRoot; scroll?: boolean; className?: string;
}) {
  // A tab's own screen has no back arrow (switching computers is in More).
  const back = backProp ?? !tab;
  return (
    <div className="flex h-full flex-col">
      <header className="flex shrink-0 items-center gap-1 border-b border-line px-2 pb-2" style={{ paddingTop: "max(10px, env(safe-area-inset-top))" }}>
        {back ? (
          <button aria-label="Back" onClick={() => nav.back()} className="-ml-0.5 flex size-9 items-center justify-center rounded-full text-muted active:bg-hover"><ChevronLeft className="size-6" /></button>
        ) : <span className="w-1.5" />}
        <div className="min-w-0 flex-1">
          <div className="truncate text-[17px] font-semibold">{title}</div>
          {sub && <div className="truncate text-[11.5px] text-muted">{sub}</div>}
        </div>
        {right && <div className="flex shrink-0 items-center gap-1 pr-1">{right}</div>}
      </header>
      <main className={clsx("min-h-0 flex-1", scroll ? "overflow-y-auto overscroll-contain" : "flex flex-col overflow-hidden", className ?? "space-y-2.5 p-3")}>{children}</main>
      {tab && <TabBar active={tab} />}
    </div>
  );
}

export function Section({ children, right }: { children: ReactNode; right?: ReactNode }) {
  return (
    <div className="flex items-center gap-2 pt-1.5 text-[10.5px] font-semibold tracking-wider text-faint uppercase">
      {children}<span className="h-px flex-1 bg-line" />{right}
    </div>
  );
}

export function Item({ icon, title, meta, right, onClick, tone, chevron, className }: {
  icon?: ReactNode; title: ReactNode; meta?: ReactNode; right?: ReactNode; onClick?: () => void; tone?: "warn" | "danger"; chevron?: boolean; className?: string;
}) {
  const Tag = onClick ? "button" : "div";
  return (
    <Tag
      onClick={onClick}
      className={clsx(
        "flex w-full items-center gap-3 rounded-xl border bg-raised px-3 py-2.5 text-left",
        tone === "warn" ? "border-warn/50 bg-warn/[0.07]" : tone === "danger" ? "border-danger/40" : "border-transparent",
        onClick && "active:bg-hover",
        className,
      )}
    >
      {icon && <span className="flex shrink-0 items-center text-muted">{icon}</span>}
      <span className="min-w-0 flex-1">
        <span className="block truncate font-semibold">{title}</span>
        {meta && <span className="block truncate text-[11.5px] text-muted">{meta}</span>}
      </span>
      {right}
      {chevron && <ChevronRight className="size-4 shrink-0 text-faint" />}
    </Tag>
  );
}

export function Card({ children, className, onClick, tone }: { children: ReactNode; className?: string; onClick?: () => void; tone?: "warn" | "danger" }) {
  const Tag = onClick ? "button" : "div";
  return (
    <Tag onClick={onClick} className={clsx("block w-full rounded-xl border bg-raised px-3 py-2.5 text-left", tone === "warn" ? "border-warn/50 bg-warn/[0.07]" : tone === "danger" ? "border-danger/40" : "border-line", onClick && "active:bg-hover", className)}>
      {children}
    </Tag>
  );
}

export function Btn({ variant = "default", size = "md", block, loading, className, children, ...p }:
  ButtonHTMLAttributes<HTMLButtonElement> & { variant?: "default" | "primary" | "danger" | "ghost"; size?: "sm" | "md"; block?: boolean; loading?: boolean }) {
  return (
    <button
      {...p}
      disabled={p.disabled || loading}
      className={clsx(
        "inline-flex items-center justify-center gap-1.5 rounded-xl font-semibold disabled:opacity-50",
        size === "sm" ? "h-8 px-3 text-[12.5px]" : "h-11 px-4 text-[14px]",
        block && "flex w-full",
        variant === "primary" && "bg-accent text-accent-fg active:brightness-90",
        variant === "default" && "border border-line bg-hover active:bg-line",
        variant === "danger" && "border border-danger/40 bg-danger/10 text-danger active:bg-danger/20",
        variant === "ghost" && "text-muted active:bg-hover",
        className,
      )}
    >
      {loading && <Loader2 className="size-4 animate-spin" />}
      {children}
    </button>
  );
}

export function IconBtn({ children, label, onClick, className }: { children: ReactNode; label: string; onClick?: () => void; className?: string }) {
  return <button aria-label={label} title={label} onClick={onClick} className={clsx("flex size-9 items-center justify-center rounded-full text-muted active:bg-hover", className)}>{children}</button>;
}

export function Toggle({ on, onChange, disabled }: { on: boolean; onChange: (v: boolean) => void; disabled?: boolean }) {
  return (
    <button role="switch" aria-checked={on} disabled={disabled} onClick={(e) => { e.stopPropagation(); onChange(!on); }}
      className={clsx("relative h-7 w-12 shrink-0 rounded-full border transition-colors disabled:opacity-50", on ? "border-ok bg-ok" : "border-line bg-hover")}>
      <span className={clsx("absolute top-[3px] size-5 rounded-full transition-all", on ? "left-[23px] bg-white" : "left-[3px] bg-muted")} />
    </button>
  );
}

export function Seg<T extends string>({ value, options, onChange }: { value: T; options: { value: T; label: ReactNode }[]; onChange: (v: T) => void }) {
  return (
    <div className="flex gap-1 rounded-xl border border-line bg-panel p-1">
      {options.map((o) => (
        <button key={o.value} onClick={() => onChange(o.value)} className={clsx("min-w-0 flex-1 truncate rounded-lg px-2 py-1.5 text-[12.5px]", value === o.value ? "bg-hover font-semibold text-fg" : "text-muted")}>
          {o.label}
        </button>
      ))}
    </div>
  );
}

const field = "w-full rounded-xl border border-line bg-panel px-3 text-[14px] text-fg placeholder:text-faint focus:border-accent focus:outline-none";

export function Input({ className, ...p }: InputHTMLAttributes<HTMLInputElement>) {
  return <input autoCapitalize="off" autoCorrect="off" spellCheck={false} {...p} className={clsx(field, "h-11", className)} />;
}

export function TextArea({ className, ...p }: TextareaHTMLAttributes<HTMLTextAreaElement>) {
  return <textarea autoCapitalize="off" autoCorrect="off" spellCheck={false} {...p} className={clsx(field, "py-2.5", className)} />;
}

export function Field({ label, hint, children }: { label: ReactNode; hint?: ReactNode; children: ReactNode }) {
  return (
    <label className="block space-y-1">
      <span className="block text-[11.5px] text-muted">{label}</span>
      {children}
      {hint && <span className="block text-[11px] text-faint">{hint}</span>}
    </label>
  );
}

export function Spinner({ className }: { className?: string }) {
  return <Loader2 className={clsx("size-5 animate-spin text-muted", className)} />;
}

export function Loading({ text = "Loading…" }: { text?: string }) {
  return <div className="flex items-center justify-center gap-2 py-10 text-muted"><Spinner />{text}</div>;
}

export function Empty({ icon, title, children }: { icon?: ReactNode; title: string; children?: ReactNode }) {
  return (
    <div className="flex flex-col items-center gap-2 px-6 py-10 text-center text-muted">
      {icon && <div className="text-faint">{icon}</div>}
      <div className="font-semibold text-fg/80">{title}</div>
      {children && <div className="text-[12.5px]">{children}</div>}
    </div>
  );
}

export function ErrorBox({ error, onRetry }: { error: string; onRetry?: () => void }) {
  return (
    <div className="selectable space-y-2 rounded-xl border border-danger/40 bg-danger/10 p-3 text-[12.5px] text-danger">
      <div className="break-words whitespace-pre-wrap">{error}</div>
      {onRetry && <Btn size="sm" onClick={onRetry}>Try again</Btn>}
    </div>
  );
}

export function Note({ children }: { children: ReactNode }) {
  return <div className="rounded-xl border border-dashed border-line px-3 py-2 text-[12px] text-muted [&_b]:text-fg">{children}</div>;
}

export function Badge({ children, tone = "default" }: { children: ReactNode; tone?: "default" | "ok" | "warn" | "danger" | "info" | "accent" }) {
  return (
    <span className={clsx(
      "inline-flex shrink-0 items-center rounded-full px-2 py-px text-[10.5px] font-semibold",
      tone === "default" && "bg-hover text-muted", tone === "ok" && "bg-ok/15 text-ok", tone === "warn" && "bg-warn/15 text-warn",
      tone === "danger" && "bg-danger/15 text-danger", tone === "info" && "bg-info/15 text-info", tone === "accent" && "bg-accent/15 text-accent",
    )}>{children}</span>
  );
}

export function Dot({ tone }: { tone: "ok" | "off" | "warn" | "danger" }) {
  return <span className={clsx("inline-block size-2 shrink-0 rounded-full", tone === "ok" ? "bg-ok" : tone === "warn" ? "bg-warn" : tone === "danger" ? "bg-danger" : "bg-faint")} />;
}

/** Bottom sheet over the current screen. */
export function Sheet({ open, onClose, title, children }: { open: boolean; onClose: () => void; title?: ReactNode; children: ReactNode }) {
  if (!open) return null;
  return (
    <div className="fixed inset-0 z-40 flex flex-col justify-end bg-black/55" onClick={onClose}>
      <div className="max-h-[88%] space-y-2.5 overflow-y-auto rounded-t-2xl border-t border-line bg-panel px-4 pt-2" style={{ paddingBottom: "max(20px, env(safe-area-inset-bottom))" }} onClick={(e) => e.stopPropagation()}>
        <div className="mx-auto mb-1 h-1 w-10 rounded-full bg-line" />
        {title && <div className="text-[16px] font-semibold">{title}</div>}
        {children}
      </div>
    </div>
  );
}

/** Ask before something that can't be undone. `typeToConfirm`: the word to type first. */
export function Confirm({ open, title, children, confirm = "Confirm", danger, typeToConfirm, onConfirm, onCancel }: {
  open: boolean; title: ReactNode; children?: ReactNode; confirm?: string; danger?: boolean; typeToConfirm?: string; onConfirm: () => void; onCancel: () => void;
}) {
  const [typed, setTyped] = useState("");
  useEffect(() => { if (open) setTyped(""); }, [open]);
  if (!open) return null;
  const ok = !typeToConfirm || typed.trim() === typeToConfirm;
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 px-5" onClick={onCancel}>
      <div className="w-full max-w-sm space-y-3 rounded-2xl border border-line bg-panel p-4" onClick={(e) => e.stopPropagation()}>
        <div className={clsx("text-[16px] font-semibold", danger && "text-warn")}>{title}</div>
        {children && <div className="text-[13px] text-muted [&_b]:text-fg">{children}</div>}
        {typeToConfirm && (
          <Field label={<>Type <b className="font-mono text-fg">{typeToConfirm}</b> to confirm</>}>
            <Input value={typed} onChange={(e) => setTyped(e.target.value)} autoFocus />
          </Field>
        )}
        <div className="flex gap-2">
          <Btn className="flex-1" onClick={onCancel}>Cancel</Btn>
          <Btn className="flex-1" variant={danger ? "danger" : "primary"} disabled={!ok} onClick={onConfirm}>{confirm}</Btn>
        </div>
      </div>
    </div>
  );
}

export function Toasts() {
  const toasts = useApp((s) => s.toasts);
  return (
    <div className="pointer-events-none fixed inset-x-0 z-[60] flex flex-col items-center gap-2 px-4" style={{ bottom: "calc(max(10px, env(safe-area-inset-bottom)) + 72px)" }}>
      {toasts.map((t) => (
        <div key={t.id} className={clsx("max-w-full rounded-xl border px-3.5 py-2 text-[13px] shadow-xl backdrop-blur", t.tone === "err" ? "border-danger/40 bg-[#2a1416]/95 text-danger" : t.tone === "ok" ? "border-ok/40 bg-[#122219]/95 text-ok" : "border-line bg-raised/95 text-fg")}>
          {t.text}
        </div>
      ))}
    </div>
  );
}

/** Load something from the computer, with loading / error state and a reload function. */
export function useLoad<T>(fn: () => Promise<T>, deps: unknown[], pollMs?: number): { data: T | undefined; error: string | null; loading: boolean; reload: () => void } {
  const [data, setData] = useState<T | undefined>(undefined);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [n, setN] = useState(0);
  useEffect(() => {
    let alive = true;
    const run = (first: boolean) => {
      if (first) setLoading(true);
      fn().then((d) => { if (alive) { setData(d); setError(null); } })
        .catch((e) => { if (alive) setError(typeof e === "string" ? e : e instanceof Error ? e.message : JSON.stringify(e)); })
        .finally(() => { if (alive) setLoading(false); });
    };
    run(true);
    const t = pollMs ? setInterval(() => { if (!document.hidden) run(false); }, pollMs) : undefined;
    return () => { alive = false; if (t) clearInterval(t); };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, n]);
  return { data, error, loading, reload: () => setN((x) => x + 1) };
}

export function ago(ms: number | string | null | undefined): string {
  if (ms == null) return "";
  const t = typeof ms === "string" ? Date.parse(ms) : ms;
  const s = Math.max(0, Math.round((Date.now() - t) / 1000));
  if (s < 45) return "just now";
  if (s < 3600) return `${Math.round(s / 60)}m ago`;
  if (s < 86400) return `${Math.round(s / 3600)}h ago`;
  return `${Math.round(s / 86400)}d ago`;
}

export function duration(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, "0")}s`;
  return `${Math.floor(s / 3600)}h ${String(Math.floor((s % 3600) / 60)).padStart(2, "0")}m`;
}

export const folderName = (p?: string | null) => (p ?? "").split(/[\\/]/).filter(Boolean).pop() ?? (p || "");
