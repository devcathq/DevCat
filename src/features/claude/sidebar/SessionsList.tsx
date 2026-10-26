// Past Claude Code sessions for the workspace folder (from ~/.claude/projects), newest first.
import { homeDir, join } from "@tauri-apps/api/path";
import clsx from "clsx";
import { AlertTriangle, Copy, FileText, GitBranch, MessageSquare, Pencil, SquareArrowOutUpRight, X } from "lucide-react";
import { memo, useMemo, useRef, useState, type KeyboardEvent, type MouseEvent } from "react";
import { toast } from "sonner";
import { Spinner } from "../../../components/ui";
import { api } from "../../../lib/api";
import type { SessionMeta } from "../../../lib/types";
import { useTabs } from "../../../store/tabs";
import { useContextMenu, type MenuItem } from "../../connections/ContextMenu";
import { copyText, relTime, revealPath } from "../../files/format";
import { chatTabTitle, claudeTabKey, openClaudeWorkspace } from "../open";
import { useClaude } from "../store";

/** ~/.claude/projects/<cwd with every non-alphanumeric char replaced by "-">/<id>.jsonl */
async function transcriptPath(cwd: string, id: string): Promise<string> {
  return join(await homeDir(), ".claude", "projects", cwd.replace(/[^a-zA-Z0-9]/g, "-"), `${id}.jsonl`);
}

function fmtCost(c?: number | null): string | null {
  if (c == null || c <= 0) return null;
  return c < 0.01 ? "<$0.01" : `$${c.toFixed(c < 10 ? 2 : 0)}`;
}

export function SessionsList({ cwd, sessions, error, loading, query, setQuery, now, onRenamed }: {
  cwd: string;
  sessions: SessionMeta[] | null;
  error: string | null;
  loading: boolean;
  query: string;
  setQuery: (q: string) => void;
  now: number;
  onRenamed: (id: string, title: string) => void;
}) {
  const menu = useContextMenu();
  const activeId = useTabs((s) => s.activeId);
  // Rename…: the name turns into a field (Enter / click away saves, Esc cancels, empty = Claude's own title)
  const [editing, setEditing] = useState<string | null>(null);
  const cancelled = useRef(false);
  const rename = async (s: SessionMeta, value: string) => {
    setEditing(null);
    const name = value.trim().split(/\s+/).join(" ");
    if (cancelled.current || name === s.title) return;
    try {
      const title = await api.claudeRename(cwd, s.id, name);
      onRenamed(s.id, title);
      // its open tab takes the new name too (a new chat learns its session once it has one)
      const tabs = useTabs.getState();
      const running = useClaude.getState().tabs;
      for (const t of tabs.tabs) {
        if (t.type === "claude" && (t.sessionId === s.id || running[t.id]?.sessionId === s.id)) tabs.update(t.id, { title: chatTabTitle(t.cwd, title) } as never);
      }
    } catch (e) {
      toast.error(`Couldn't rename the session: ${String(e)}`);
    }
  };

  const filtered = useMemo(() => {
    const list = [...(sessions ?? [])].sort((a, b) => (b.updatedAt ?? "").localeCompare(a.updatedAt ?? ""));
    const q = query.trim().toLowerCase();
    if (!q) return list;
    return list.filter((s) =>
      s.title.toLowerCase().includes(q) || (s.firstPrompt ?? "").toLowerCase().includes(q) || (s.lastPrompt ?? "").toLowerCase().includes(q)
      || (s.gitBranch ?? "").toLowerCase().includes(q) || s.id.startsWith(q));
  }, [sessions, query]);

  const open = (s: SessionMeta) => openClaudeWorkspace(cwd, { sessionId: s.id, title: s.title });

  const showMenu = (e: MouseEvent, s: SessionMeta) => {
    const items: MenuItem[] = [
      { label: "Open", icon: <SquareArrowOutUpRight className="size-3.5" />, onClick: () => open(s) },
      { label: "Rename…", icon: <Pencil className="size-3.5" />, onClick: () => { cancelled.current = false; setEditing(s.id); } },
      "separator",
      { label: "Copy session id", icon: <Copy className="size-3.5" />, onClick: () => void copyText(s.id) },
      { label: "Copy resume command", icon: <Copy className="size-3.5" />, onClick: () => void copyText(`claude --resume ${s.id}`) },
      {
        label: "Reveal transcript file", icon: <FileText className="size-3.5" />,
        onClick: () => void transcriptPath(cwd, s.id).then(revealPath).catch((err) => toast.error(String(err))),
      },
    ];
    menu.show(e, items);
  };

  const onKey = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    const rows = Array.from(e.currentTarget.querySelectorAll<HTMLElement>("[data-session-row]"));
    const i = rows.indexOf(document.activeElement as HTMLElement);
    const next = rows[e.key === "ArrowDown" ? Math.min(rows.length - 1, i + 1) : Math.max(0, i - 1)];
    if (next) { e.preventDefault(); next.focus(); }
  };

  return (
    <div className="flex h-full min-h-0 flex-col">
      {(sessions?.length ?? 0) > 6 && (
        <div className="relative shrink-0 px-2 pb-1">
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => e.key === "Escape" && setQuery("")}
            placeholder="Search sessions"
            spellCheck={false}
            className="h-5.5 w-full rounded border border-line bg-app px-2 pr-5 text-[12px] placeholder:text-faint focus:border-accent focus:outline-none"
          />
          {query && (
            <button className="absolute top-1/2 right-3 -translate-y-1/2 text-faint hover:text-fg" onClick={() => setQuery("")} aria-label="Clear search">
              <X className="size-3" />
            </button>
          )}
        </div>
      )}
      <div className="min-h-0 flex-1 overflow-y-auto pb-1" onKeyDown={onKey}>
        {error ? (
          <div className="flex items-start gap-1.5 px-3 py-1.5 text-[12px] text-danger/90" title={error}>
            <AlertTriangle className="mt-0.5 size-3 shrink-0" />
            <span className="line-clamp-3 break-words">{error}</span>
          </div>
        ) : sessions === null ? (
          loading && <div className="flex items-center gap-2 px-3 py-1.5 text-[12px] text-faint"><Spinner className="size-3" /> Loading sessions…</div>
        ) : filtered.length === 0 ? (
          <div className="px-3 py-1.5 text-[12px] text-faint italic">{query ? "No matching session" : "No sessions yet in this folder"}</div>
        ) : (
          filtered.map((s) => editing === s.id ? (
            <div key={s.id} className="mx-1 px-1 py-1">
              <input
                autoFocus
                defaultValue={s.title}
                aria-label="Session name"
                placeholder="Claude's own title"
                spellCheck={false}
                onFocus={(e) => e.currentTarget.select()}
                onKeyDown={(e) => {
                  e.stopPropagation();
                  if (e.key === "Enter") e.currentTarget.blur();
                  else if (e.key === "Escape") { cancelled.current = true; e.currentTarget.blur(); }
                }}
                onBlur={(e) => void rename(s, e.currentTarget.value)}
                className="h-6.5 w-full rounded border border-accent bg-app px-1.5 text-[13px] text-fg placeholder:text-faint focus:outline-none"
              />
            </div>
          ) : (
            <SessionRow key={s.id} s={s} now={now} active={activeId === claudeTabKey(cwd, s.id)} onOpen={open} onMenu={showMenu} />
          ))
        )}
      </div>
      {menu.element}
    </div>
  );
}

const SessionRow = memo(function SessionRow({ s, now, active, onOpen, onMenu }: {
  s: SessionMeta;
  now: number;
  active: boolean;
  onOpen: (s: SessionMeta) => void;
  onMenu: (e: MouseEvent, s: SessionMeta) => void;
}) {
  const cost = fmtCost(s.costUsd);
  const title = s.title.trim() || s.firstPrompt?.trim() || "(untitled session)";
  return (
    <div
      data-session-row
      tabIndex={0}
      className={clsx(
        "mx-1 cursor-default rounded px-2 py-1 outline-none focus-visible:bg-hover",
        active ? "bg-accent/15" : "hover:bg-hover/70",
      )}
      title={`${title}\n${s.id}${s.lastPrompt ? `\nLast: ${s.lastPrompt.slice(0, 200)}` : ""}`}
      onClick={() => onOpen(s)}
      onKeyDown={(e) => e.key === "Enter" && onOpen(s)}
      onContextMenu={(e) => onMenu(e, s)}
    >
      <div className="truncate text-[13px] text-fg/90">{title}</div>
      <div className="mt-0.5 flex items-center gap-2 text-[11px] text-faint">
        <span className="shrink-0">{relTime(s.updatedAt ?? s.createdAt, now)}</span>
        <span className="inline-flex shrink-0 items-center gap-0.5"><MessageSquare className="size-2.5" />{s.messageCount}</span>
        {s.gitBranch && (
          <span className="inline-flex min-w-0 items-center gap-0.5 truncate"><GitBranch className="size-2.5 shrink-0" /><span className="truncate">{s.gitBranch}</span></span>
        )}
        {cost && <span className="ml-auto shrink-0 font-mono">{cost}</span>}
      </div>
    </div>
  );
});
