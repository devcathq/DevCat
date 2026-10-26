// Lightweight context menu. Usage:
//   const menu = useContextMenu();
//   <div onContextMenu={(e) => menu.show(e, items)} />  ...  {menu.element}
import clsx from "clsx";
import { ChevronRight } from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type MouseEvent, type ReactNode } from "react";

export type MenuItem =
  | { label: string; icon?: ReactNode; shortcut?: string; danger?: boolean; disabled?: boolean; onClick: () => void }
  | { label: string; icon?: ReactNode; submenu: MenuItem[] }
  | "separator";

export function useContextMenu() {
  const [state, setState] = useState<{ x: number; y: number; items: MenuItem[] } | null>(null);
  const show = useCallback((e: MouseEvent | { clientX: number; clientY: number; preventDefault?: () => void }, items: MenuItem[]) => {
    e.preventDefault?.();
    if ("stopPropagation" in e) e.stopPropagation();
    setState({ x: e.clientX, y: e.clientY, items });
  }, []);
  const close = useCallback(() => setState(null), []);
  const element = state ? <ContextMenu x={state.x} y={state.y} items={state.items} onClose={close} /> : null;
  return { show, close, element };
}

export function ContextMenu({ x, y, items, onClose }: { x: number; y: number; items: MenuItem[]; onClose: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ x, y });
  const [active, setActive] = useState(-1);
  const [openSub, setOpenSub] = useState<number | null>(null);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    setPos({
      x: Math.max(4, Math.min(x, window.innerWidth - r.width - 4)),
      y: Math.max(4, Math.min(y, window.innerHeight - r.height - 4)),
    });
  }, [x, y]);

  useEffect(() => {
    const down = (e: globalThis.MouseEvent) => {
      if (!(e.target as HTMLElement).closest("[data-ctxmenu]")) onClose();
    };
    const key = (e: KeyboardEvent) => {
      const actionable = items.map((it, i) => (it !== "separator" && !("disabled" in it && it.disabled) ? i : -1)).filter((i) => i >= 0);
      if (e.key === "Escape") { e.preventDefault(); onClose(); }
      else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        const cur = actionable.indexOf(active);
        const next = e.key === "ArrowDown" ? (cur + 1) % actionable.length : (cur - 1 + actionable.length) % actionable.length;
        setActive(actionable[next] ?? -1);
      } else if (e.key === "Enter" && active >= 0) {
        e.preventDefault();
        const it = items[active];
        if (it !== "separator" && "onClick" in it) { onClose(); it.onClick(); }
        else setOpenSub(active);
      } else if (e.key === "ArrowRight" && active >= 0) setOpenSub(active);
    };
    window.addEventListener("mousedown", down, true);
    window.addEventListener("keydown", key, true);
    window.addEventListener("blur", onClose);
    window.addEventListener("resize", onClose);
    return () => {
      window.removeEventListener("mousedown", down, true);
      window.removeEventListener("keydown", key, true);
      window.removeEventListener("blur", onClose);
      window.removeEventListener("resize", onClose);
    };
  }, [items, active, onClose]);

  return (
    <div
      ref={ref}
      data-ctxmenu
      className="fixed z-[60] min-w-48 rounded-md border border-line bg-raised py-1 shadow-2xl shadow-black/50"
      style={{ left: pos.x, top: pos.y }}
      onContextMenu={(e) => e.preventDefault()}
    >
      {items.map((it, i) =>
        it === "separator" ? (
          <div key={i} className="my-1 h-px bg-line" />
        ) : (
          <div key={i} className="relative" onMouseEnter={() => { setActive(i); setOpenSub("submenu" in it ? i : null); }}>
            <button
              disabled={"disabled" in it && it.disabled}
              onClick={() => {
                if ("onClick" in it) { onClose(); it.onClick(); }
                else setOpenSub(i);
              }}
              className={clsx(
                "flex h-7 w-full items-center gap-2 px-3 text-left text-[13px] disabled:opacity-40",
                i === active && "bg-hover",
                "danger" in it && it.danger ? "text-danger" : "text-fg",
              )}
            >
              <span className="flex size-4 items-center justify-center text-muted">{it.icon}</span>
              <span className="flex-1 truncate">{it.label}</span>
              {"shortcut" in it && it.shortcut && <span className="pl-4 text-[11px] text-faint">{it.shortcut}</span>}
              {"submenu" in it && <ChevronRight className="size-3.5 text-muted" />}
            </button>
            {"submenu" in it && openSub === i && (
              <SubMenu items={it.submenu} onClose={onClose} />
            )}
          </div>
        ),
      )}
    </div>
  );
}

function SubMenu({ items, onClose }: { items: MenuItem[]; onClose: () => void }) {
  // Opens to the left when there's no room on the right (a menu at the window's right edge).
  const ref = useRef<HTMLDivElement>(null);
  const [left, setLeft] = useState(false);
  useLayoutEffect(() => {
    const r = ref.current?.getBoundingClientRect();
    if (r && r.right > window.innerWidth - 4) setLeft(true);
  }, []);
  return (
    <div ref={ref} data-ctxmenu className={clsx("absolute top-[-4px] z-[61] min-w-44 rounded-md border border-line bg-raised py-1 shadow-2xl shadow-black/50", left ? "right-full mr-0.5" : "left-full ml-0.5")}>
      {items.map((it, i) =>
        it === "separator" ? <div key={i} className="my-1 h-px bg-line" /> : "onClick" in it ? (
          <button
            key={i}
            disabled={it.disabled}
            onClick={() => { onClose(); it.onClick(); }}
            className={clsx("flex h-7 w-full items-center gap-2 px-3 text-left text-[13px] hover:bg-hover disabled:opacity-40", it.danger ? "text-danger" : "text-fg")}
          >
            <span className="flex size-4 items-center justify-center text-muted">{it.icon}</span>
            <span className="flex-1 truncate">{it.label}</span>
          </button>
        ) : null,
      )}
    </div>
  );
}
