// Small set of shared UI primitives. Keep them dumb and composable.
import clsx from "clsx";
import { Loader2, X, type LucideIcon } from "lucide-react";
import { useEffect, useState, type ButtonHTMLAttributes, type InputHTMLAttributes, type ReactNode, type SelectHTMLAttributes, type TextareaHTMLAttributes } from "react";

type Variant = "primary" | "ghost" | "danger" | "default";

export function Button({ variant = "default", size = "md", className, loading, children, ...p }:
  ButtonHTMLAttributes<HTMLButtonElement> & { variant?: Variant; size?: "sm" | "md"; loading?: boolean }) {
  return (
    <button
      {...p}
      disabled={p.disabled || loading}
      className={clsx(
        "inline-flex items-center gap-1.5 rounded-md font-medium transition-colors disabled:opacity-50 disabled:pointer-events-none whitespace-nowrap",
        size === "sm" ? "h-6 px-2 text-xs" : "h-7 px-3 text-[13px]",
        variant === "primary" && "bg-accent text-accent-fg hover:brightness-110",
        variant === "default" && "bg-raised border border-line hover:bg-hover",
        variant === "ghost" && "hover:bg-hover text-muted hover:text-fg",
        variant === "danger" && "bg-danger/15 text-danger border border-danger/30 hover:bg-danger/25",
        className,
      )}
    >
      {loading && <Spin icon={Loader2} className="size-3.5" />}
      {children}
    </button>
  );
}

export function IconButton({ title, className, children, active, ...p }:
  ButtonHTMLAttributes<HTMLButtonElement> & { title: string; active?: boolean }) {
  return (
    <button
      {...p}
      title={title}
      aria-label={title}
      className={clsx(
        "inline-flex items-center justify-center size-6 rounded text-muted hover:text-fg hover:bg-hover disabled:opacity-40 disabled:pointer-events-none",
        active && "text-accent bg-hover",
        className,
      )}
    >
      {children}
    </button>
  );
}

const field = "h-7 rounded-md bg-app border border-line px-2 text-[13px] text-fg placeholder:text-faint focus:border-accent focus:outline-none";

/** Fields are full width unless the caller passes its own width (`w-*`, `flex-*`). */
const width = (className?: string) => (/(^|\s)!?(w-|flex-1|flex-auto|grow)/.test(className ?? "") ? undefined : "w-full");

export function Input({ className, ...p }: InputHTMLAttributes<HTMLInputElement>) {
  return <input spellCheck={false} autoCorrect="off" autoCapitalize="off" {...p} className={clsx(field, width(className), className)} />;
}

export function Textarea({ className, ...p }: TextareaHTMLAttributes<HTMLTextAreaElement>) {
  return <textarea spellCheck={false} {...p} className={clsx(field, width(className), "h-auto py-1.5 font-mono text-xs", className)} />;
}

export function Select({ className, children, ...p }: SelectHTMLAttributes<HTMLSelectElement>) {
  return <select {...p} className={clsx(field, width(className), "pr-6", className)}>{children}</select>;
}

export function Field({ label, children, hint, className }: { label: string; children: ReactNode; hint?: string; className?: string }) {
  return (
    <label className={clsx("flex flex-col gap-1", className)}>
      <span className="text-xs text-muted">{label}</span>
      {children}
      {hint && <span className="text-[11px] text-faint">{hint}</span>}
    </label>
  );
}

export function Checkbox({ label, checked, onChange }: { label: string; checked: boolean; onChange: (v: boolean) => void }) {
  return (
    <label className="inline-flex items-center gap-2 text-[13px] cursor-pointer">
      <input type="checkbox" className="accent-[var(--color-accent)]" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      {label}
    </label>
  );
}

export function Spinner({ className }: { className?: string }) {
  return <Spin icon={Loader2} className={clsx("size-4 text-muted", className)} />;
}

/** A spinning icon. Use this, never `animate-spin` on an icon: WebView2 can't hand a spinning <svg>
 *  to the GPU, so it redraws the window on every frame (about 10% of a core on a 160 Hz screen) for
 *  as long as the icon shows. The span spins instead, and the icon fills it, so `className` (size,
 *  color, margins, position) goes on the span. `spinning={false}` shows the icon still. */
export function Spin({ icon: Icon, spinning = true, className }: { icon: LucideIcon; spinning?: boolean; className?: string }) {
  return (
    <span className={clsx("flex", spinning && "animate-spin", className)}>
      <Icon className="size-full" />
    </span>
  );
}

export function Empty({ icon, title, children }: { icon?: ReactNode; title: string; children?: ReactNode }) {
  return (
    <div className="h-full w-full flex flex-col items-center justify-center gap-2 text-center p-6 text-muted">
      {icon && <div className="text-faint">{icon}</div>}
      <div className="text-fg/80 font-medium">{title}</div>
      {children && <div className="text-xs max-w-sm">{children}</div>}
    </div>
  );
}

export function ErrorBox({ error }: { error: string }) {
  return (
    <div className="selectable m-3 rounded-md border border-danger/30 bg-danger/10 p-3 font-mono text-xs text-danger whitespace-pre-wrap">
      {error}
    </div>
  );
}

/** Open dialogs, innermost last — only the topmost one reacts to Escape. */
const dialogStack: symbol[] = [];

export function Dialog({ open, onClose, title, children, footer, width = 560 }:
  { open: boolean; onClose: () => void; title: ReactNode; children: ReactNode; footer?: ReactNode; width?: number }) {
  // A dialog opened from another one (e.g. "Delete?" from a settings dialog) goes on top of it.
  const [depth, setDepth] = useState(0);
  useEffect(() => {
    if (!open) return;
    const me = Symbol("dialog");
    dialogStack.push(me);
    setDepth(dialogStack.length);
    const h = (e: KeyboardEvent) => {
      if (e.key === "Escape" && dialogStack[dialogStack.length - 1] === me) onClose();
    };
    window.addEventListener("keydown", h);
    return () => {
      window.removeEventListener("keydown", h);
      dialogStack.splice(dialogStack.indexOf(me), 1);
    };
  }, [open, onClose]);
  if (!open) return null;
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50" style={{ zIndex: 50 + depth }} onMouseDown={onClose}>
      <div
        className="flex max-h-[88vh] flex-col rounded-lg border border-line bg-panel shadow-2xl"
        style={{ width }}
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div className="flex h-10 items-center justify-between border-b border-line px-4">
          <div className="font-semibold">{title}</div>
          <IconButton title="Close" onClick={onClose}><X className="size-4" /></IconButton>
        </div>
        <div className="min-h-0 flex-1 overflow-auto p-4">{children}</div>
        {footer && <div className="flex items-center justify-end gap-2 border-t border-line px-4 py-2.5">{footer}</div>}
      </div>
    </div>
  );
}

/** Thin horizontal toolbar used at the top of tabs. */
export function Toolbar({ children, className }: { children: ReactNode; className?: string }) {
  return <div className={clsx("flex h-9 shrink-0 items-center gap-1 border-b border-line bg-panel px-2", className)}>{children}</div>;
}

export function Badge({ children, tone = "default" }: { children: ReactNode; tone?: "default" | "ok" | "warn" | "danger" | "info" | "accent" }) {
  return (
    <span className={clsx(
      "inline-flex items-center rounded px-1.5 py-px text-[11px] font-medium",
      tone === "default" && "bg-hover text-muted",
      tone === "ok" && "bg-ok/15 text-ok",
      tone === "warn" && "bg-warn/15 text-warn",
      tone === "danger" && "bg-danger/15 text-danger",
      tone === "info" && "bg-info/15 text-info",
      tone === "accent" && "bg-accent/15 text-accent",
    )}>{children}</span>
  );
}
