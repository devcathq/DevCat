// The project switcher on the tab screens (project focus, lib/focus.ts): "All projects" or one
// of the computer's projects; the screen below shows only that project's chats, terminals,
// folders and connections.
import clsx from "clsx";
import { Check, ChevronDown, Layers } from "lucide-react";
import { useEffect, useState } from "react";
import { focusedProject, loadFocus, setFocus, useFocus } from "../lib/focus";
import { useApp } from "../lib/store";
import { Note, Sheet } from "../ui";

export function ProjectBar() {
  const machine = useApp((s) => s.current);
  const online = useApp((s) => s.conn[s.current ?? ""]?.state === "online");
  const s = useFocus();
  const [open, setOpen] = useState(false);
  useEffect(() => { if (online) void loadFocus(); }, [machine, online]);
  const p = focusedProject(s);
  return (
    <>
      <button onClick={() => setOpen(true)} className={clsx("flex h-8 max-w-full items-center gap-1.5 rounded-full px-3 text-[12.5px] font-semibold active:opacity-80", p ? "bg-accent/15 text-accent" : "bg-hover text-muted")}>
        {p ? <span className="size-2.5 shrink-0 rounded-full" style={{ background: p.color ?? "var(--color-accent)" }} /> : <Layers className="size-3.5 shrink-0" />}
        <span className="truncate">{p ? p.name : "All projects"}</span>
        <ChevronDown className="size-3.5 shrink-0 opacity-70" />
      </button>
      <Sheet open={open} onClose={() => setOpen(false)} title="Focus on a project">
        <Choice on={!p} title="All projects" meta="Everything on this computer" onClick={() => { setFocus(null); setOpen(false); }} icon={<Layers className="size-4 text-muted" />} />
        {s.projects.length === 0 && <Note>This computer has no projects yet. Create them in DevCat on the computer (the project menu at the top left).</Note>}
        {s.projects.map((x) => (
          <Choice key={x.id} on={p?.id === x.id} title={x.name}
            meta={x.folders?.length ? x.folders.map((f) => f.split(/[\\/]/).filter(Boolean).pop()).join(" · ") : "No code folders yet"}
            onClick={() => { setFocus(x.id); setOpen(false); }}
            icon={<span className="size-3 rounded-full" style={{ background: x.color ?? "var(--color-accent)" }} />} />
        ))}
        <Note>Chats and terminals belong to a project by its code folders, and connections by their project. Set them in DevCat on the computer: the project's menu → <b>Code folders &amp; MCP…</b></Note>
      </Sheet>
    </>
  );
}

function Choice({ on, title, meta, icon, onClick }: { on: boolean; title: string; meta: string; icon: React.ReactNode; onClick: () => void }) {
  return (
    <button onClick={onClick} className={clsx("flex w-full items-center gap-3 rounded-xl px-3 py-3 text-left active:bg-hover", on ? "bg-accent/10 ring-1 ring-accent/60" : "bg-raised")}>
      <span className="flex size-5 shrink-0 items-center justify-center">{icon}</span>
      <span className="min-w-0 flex-1"><span className="block truncate font-semibold">{title}</span><span className="block truncate text-[11.5px] text-muted">{meta}</span></span>
      {on && <Check className="size-4 text-accent" />}
    </button>
  );
}
