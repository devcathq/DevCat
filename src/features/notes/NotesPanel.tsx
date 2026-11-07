// Sidebar: notes grouped by project. In focus mode only the focused project's notes and the
// General ones (not tied to a project) are listed.
import clsx from "clsx";
import { Copy, ExternalLink, FolderInput, Layers, MousePointerClick, NotebookPen, Pin, PinOff, Plus, Search, Trash2 } from "lucide-react";
import { useMemo, useState } from "react";
import { toast } from "sonner";
import { IconButton, Spinner } from "../../components/ui";
import { errMsg } from "../../lib/api";
import type { Note } from "../../lib/types";
import { useConnections } from "../../store/connections";
import { useTabs } from "../../store/tabs";
import { useContextMenu, type MenuItem } from "../connections/ContextMenu";
import { confirmDialog } from "../connections/dialogs";
import { copyText, relativeTime } from "../history/historyActions";
import { useNow } from "../history/HistoryPanel";
import { openReviewDialog } from "../review/ReviewUrlDialog";
import { createNote, noteSnippet, noteTitle, openNote, useNotes } from "./store";

const REVIEW_PREFIX = "UI review · ";

interface Group { id: string | null; name: string; color?: string | null; notes: Note[] }

export default function NotesPanel() {
  const notes = useNotes((s) => s.notes);
  const loaded = useNotes((s) => s.loaded);
  const error = useNotes((s) => s.error);
  const projects = useConnections((s) => s.projects);
  const focus = useConnections((s) => s.focusProjectId);
  const activeId = useTabs((s) => s.activeId);
  const [search, setSearch] = useState("");
  const menu = useContextMenu();
  useNow(60_000);

  const groups = useMemo((): Group[] => {
    const q = search.trim().toLowerCase();
    const hit = (n: Note) => !q || n.body.toLowerCase().includes(q);
    const known = new Set(projects.map((p) => p.id));
    // a note whose project is gone counts as General
    const general = notes.filter((n) => hit(n) && (!n.projectId || !known.has(n.projectId)));
    const ofProject = (id: string) => notes.filter((n) => hit(n) && n.projectId === id);
    const g: Group[] = focus
      ? projects.filter((p) => p.id === focus).map((p) => ({ id: p.id, name: p.name, color: p.color, notes: ofProject(p.id) }))
      : projects.map((p) => ({ id: p.id, name: p.name, color: p.color, notes: ofProject(p.id) })).filter((x) => x.notes.length > 0);
    return [...g, { id: null, name: "General", notes: general }];
  }, [notes, projects, focus, search]);

  const move = async (n: Note, projectId: string | null) => {
    try { await useNotes.getState().save({ ...n, projectId }); } catch (e) { toast.error("Could not move the note", { description: errMsg(e) }); }
  };
  const remove = async (n: Note) => {
    const ok = !n.body.trim() || await confirmDialog({
      title: "Delete note?",
      message: <>"<b>{noteTitle(n.body)}</b>" will be deleted. This can't be undone.</>,
      confirmLabel: "Delete",
      danger: true,
    });
    if (!ok) return;
    try {
      await useNotes.getState().remove(n.id);
      useTabs.getState().close(`note:${n.id}`, true);
    } catch (e) {
      toast.error("Could not delete the note", { description: errMsg(e) });
    }
  };

  const noteMenu = (n: Note): MenuItem[] => [
    { label: "Open", icon: <ExternalLink className="size-3.5" />, onClick: () => openNote(n) },
    { label: n.pinned ? "Unpin" : "Pin to the top", icon: n.pinned ? <PinOff className="size-3.5" /> : <Pin className="size-3.5" />, onClick: () => void useNotes.getState().save({ ...n, pinned: !n.pinned }) },
    {
      label: "Move to", icon: <FolderInput className="size-3.5" />, submenu: [
        { label: "General", icon: <Layers className="size-3.5" />, disabled: !n.projectId, onClick: () => void move(n, null) },
        ...(projects.length ? ["separator" as const] : []),
        ...projects.map((p): MenuItem => ({
          label: p.name,
          icon: <span className="size-2 rounded-full" style={{ background: p.color ?? "var(--color-faint)" }} />,
          disabled: n.projectId === p.id,
          onClick: () => void move(n, p.id),
        })),
      ],
    },
    { label: "Copy text", icon: <Copy className="size-3.5" />, onClick: () => copyText(n.body, "note") },
    "separator",
    { label: "Delete", icon: <Trash2 className="size-3.5" />, danger: true, onClick: () => void remove(n) },
  ];

  const total = groups.reduce((a, g) => a + g.notes.length, 0);
  return (
    <div className="flex h-full flex-col">
      <div className="flex h-9 shrink-0 items-center gap-1 border-b border-line px-2">
        <div className="relative min-w-0 flex-1">
          <Search className="pointer-events-none absolute top-1/2 left-2 size-3.5 -translate-y-1/2 text-faint" />
          <input
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            onKeyDown={(e) => e.key === "Escape" && setSearch("")}
            placeholder="Search notes"
            spellCheck={false}
            className="h-6 w-full rounded border border-line bg-app pr-2 pl-7 text-xs text-fg placeholder:text-faint focus:border-accent focus:outline-none"
          />
        </div>
        <IconButton title="Review page…" onClick={openReviewDialog}>
          <MousePointerClick className="size-3.5" />
        </IconButton>
      </div>
      {error && <div className="selectable border-b border-line bg-danger/10 px-3 py-2 text-xs text-danger">{error}</div>}
      <div className="min-h-0 flex-1 overflow-y-auto">
        {!loaded ? (
          <div className="flex justify-center p-4"><Spinner /></div>
        ) : groups.map((g) => (
          <div key={g.id ?? "general"}>
            <div className="group sticky top-0 z-10 flex h-7 items-center gap-1.5 border-b border-line bg-panel px-3 text-[11px] font-semibold tracking-wider text-muted uppercase">
              {g.id ? <span className="size-2 shrink-0 rounded-full" style={{ background: g.color ?? "var(--color-faint)" }} /> : <Layers className="size-3 shrink-0" />}
              <span className="min-w-0 flex-1 truncate" title={g.id ? undefined : "Not tied to a project; shown in every project"}>{g.name}</span>
              <span className="font-normal tabular-nums text-faint">{g.notes.length}</span>
              <button
                title={`New note in ${g.name}`}
                onClick={() => void createNote(g.id)}
                className="rounded p-0.5 text-faint opacity-0 group-hover:opacity-100 hover:bg-hover hover:text-fg"
              >
                <Plus className="size-3.5" />
              </button>
            </div>
            {g.notes.length === 0 ? (
              <div className="px-3 py-2 text-xs text-faint">
                {search ? "No matching notes." : g.id ? "No notes for this project yet." : "Notes here show in every project."}
              </div>
            ) : g.notes.map((n) => {
              const snippet = noteSnippet(n.body);
              const isReview = n.body.startsWith(REVIEW_PREFIX);
              return (
                <div key={n.id} className="relative">
                <button
                  onClick={() => openNote(n)}
                  onContextMenu={(e) => menu.show(e, noteMenu(n))}
                  className={clsx("flex w-full flex-col gap-0.5 border-b border-line/40 px-3 py-1.5 text-left hover:bg-hover", activeId === `note:${n.id}` && "bg-accent/10", isReview && "pb-6")}
                >
                  <span className="flex w-full items-center gap-1.5">
                    {n.pinned ? <Pin className="size-3 shrink-0 text-accent" /> : <NotebookPen className="size-3 shrink-0 text-faint" />}
                    <span className={clsx("min-w-0 flex-1 truncate text-[13px]", !n.body.trim() && "text-faint italic")}>{noteTitle(n.body)}</span>
                    <span className="shrink-0 text-[11px] text-faint">{relativeTime(new Date(n.updatedAt).toISOString())}</span>
                  </span>
                  {snippet && <span className="truncate pl-[18px] text-[11px] text-muted">{snippet}</span>}
                </button>
                {isReview && (
                  <button
                    title="Copy review"
                    onClick={(e) => { e.stopPropagation(); void copyText(n.body, "review"); }}
                    className="absolute right-2 bottom-1 flex items-center gap-1 rounded px-1 py-0.5 text-[11px] text-muted hover:bg-hover hover:text-fg"
                  >
                    <Copy className="size-3" /> Copy review
                  </button>
                )}
                </div>
              );
            })}
          </div>
        ))}
        {loaded && total === 0 && !search && (
          <div className="flex flex-col items-center gap-2 px-6 py-8 text-center text-xs text-faint">
            <NotebookPen className="size-6" />
            Keep notes per project: todos, commands, links, anything. Markdown is supported.
            <button onClick={() => void createNote()} className="mt-1 rounded-md border border-line px-2.5 py-1 text-fg hover:bg-hover">New note</button>
          </div>
        )}
      </div>
      {menu.element}
    </div>
  );
}
