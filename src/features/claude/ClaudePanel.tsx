// Sidebar for the AI workspace activity: workspace folder, past sessions (Claude Code), file tree.
// The AI connection new chats use is picked in the sidebar header (ProviderPicker).
import clsx from "clsx";
import {
  ChevronDown, ChevronRight, Copy, FolderGit2, FolderOpen, FolderSearch, GitBranch, History, MessageSquare, Pencil, Plus, RefreshCw, Sparkles, Terminal, X, Server, SquareTerminal,
} from "lucide-react";
import { AiTerminalIcon } from "../../components/ui/AppIcons";
import { useCallback, useEffect, useRef, useState, type MouseEvent, type ReactNode } from "react";
import { Group, Panel, Separator, usePanelRef } from "react-resizable-panels";
import { toast } from "sonner";
import { Button, IconButton, Spinner } from "../../components/ui";
import { api } from "../../lib/api";
import type { ClaudeInfo, GitStatus, SessionMeta } from "../../lib/types";
import { useContextMenu, type MenuItem } from "../connections/ContextMenu";
import { basename, copyText, relTime, revealPath, REVEAL_LABEL } from "../files/format";
import { openGitRepo } from "../git/open";
import { openClaudeWorkspace } from "./open";
import { useSelectedProvider } from "./providers";
import { renameChat, useAgent } from "../agent/store";
import { AgentJobs } from "../agent/Jobs";
import FileTree, { type FileTreeHandle } from "./sidebar/FileTree";
import { SessionsList } from "./sidebar/SessionsList";
import { pickFolder, useWorkspace } from "./sidebar/workspace";
import { RemoteFolderDialog, openRemoteFolderDialog } from "./sidebar/RemoteFolderDialog";
import { openTerminal } from "../terminal/open";
import { displayPath, isRemote, remoteParts } from "../../lib/remotePath";

const REFRESH_MS = 15_000;
const INSTALL_CMD = "curl -fsSL https://claude.ai/install.sh | bash";

/** Runs `fn` every `ms` while the window is visible (and once when it becomes visible again). */
function useVisibleInterval(fn: () => void, ms: number) {
  const ref = useRef(fn);
  ref.current = fn;
  useEffect(() => {
    const t = setInterval(() => { if (!document.hidden) ref.current(); }, ms);
    const vis = () => { if (!document.hidden) ref.current(); };
    document.addEventListener("visibilitychange", vis);
    return () => { clearInterval(t); document.removeEventListener("visibilitychange", vis); };
  }, [ms]);
}

/** Shorten a path for display: /Users/me/x → ~/x, ssh://devbox/home/me/x → devbox: ~/x */
const tildify = displayPath;

export default function ClaudePanel() {
  const cwd = useWorkspace((s) => s.cwd);
  const [info, setInfo] = useState<ClaudeInfo | null>(null);
  const isClaude = useSelectedProvider().kind === "claude";

  useEffect(() => {
    if (isClaude) api.claudeInfo().then(setInfo).catch(() => setInfo(null));
  }, [isClaude]);

  return (
    <div className="flex h-full flex-col">
      <WorkspaceHeader />
      {isClaude && info && !info.path && <InstallBanner />}
      {cwd ? <Workspace key={cwd} cwd={cwd} /> : <NoFolder />}
    </div>
  );
}

function InstallBanner() {
  return (
    <div className="mx-2 mt-2 rounded-md border border-warn/30 bg-warn/10 p-2 text-[12px]">
      <div className="flex items-center gap-1.5 font-medium text-warn"><Terminal className="size-3.5" /> Claude Code CLI not found</div>
      <div className="mt-1 text-muted">Install it, then restart DevCat:</div>
      <button
        className="selectable mt-1 flex w-full items-center gap-1 rounded bg-app px-1.5 py-1 text-left font-mono text-[11px] text-fg/90 hover:bg-hover"
        title="Copy command"
        onClick={() => void copyText(INSTALL_CMD, "Install command copied")}
      >
        <span className="min-w-0 flex-1 truncate">{INSTALL_CMD}</span>
        <Copy className="size-3 shrink-0 text-faint" />
      </button>
    </div>
  );
}

function NoFolder() {
  const provider = useSelectedProvider();
  return (
    <div className="flex flex-1 flex-col items-center justify-center gap-3 p-6 text-center">
      <AiTerminalIcon className="size-8 text-accent/70" />
      <div className="font-medium text-fg/85">No folder open</div>
      <div className="max-w-56 text-xs text-muted">
        Pick a project folder to chat with {provider.name} about it{provider.kind === "claude" ? ", browse its files and resume past sessions" : " and browse its files"}.
      </div>
      <Button variant="primary" onClick={() => void pickFolder()}><FolderOpen className="size-3.5" /> Open folder…</Button>
      <Button variant="ghost" onClick={openRemoteFolderDialog}><Server className="size-3.5" /> Folder on SSH host…</Button>
    </div>
  );
}

/** Current folder + picker menu (open…, recents, projects with Claude history). */
function WorkspaceHeader() {
  const cwd = useWorkspace((s) => s.cwd);
  const recent = useWorkspace((s) => s.recent);
  const setCwd = useWorkspace((s) => s.setCwd);
  const forgetRecent = useWorkspace((s) => s.forgetRecent);
  const branch = useGitBranch(cwd);
  const gitRoot = useSharedGit(cwd)?.root ?? null;
  const provider = useSelectedProvider();
  const isClaude = provider.kind === "claude";
  const menu = useContextMenu();

  const showMenu = async (e: MouseEvent) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const at = { clientX: r.left, clientY: r.bottom + 2 };
    let projects: MenuItem[] = [];
    try {
      const list = await api.claudeProjects();
      projects = [...list]
        .sort((a, b) => (b.lastActive ?? "").localeCompare(a.lastActive ?? ""))
        .slice(0, 20)
        .map((p) => ({
          label: `${tildify(p.cwd)}  ·  ${p.sessionCount} session${p.sessionCount === 1 ? "" : "s"}${p.lastActive ? ` · ${relTime(p.lastActive)}` : ""}`,
          icon: <History className="size-3.5" />,
          onClick: () => setCwd(p.cwd),
        }));
    } catch {
      projects = [{ label: "Unavailable", disabled: true, onClick: () => {} }];
    }
    const items: MenuItem[] = [
      { label: "Open folder…", icon: <FolderOpen className="size-3.5" />, onClick: () => void pickFolder() },
      { label: "Open folder on SSH host…", icon: <Server className="size-3.5" />, onClick: openRemoteFolderDialog },
      ...(isClaude
        ? [{ label: "Projects with Claude history", icon: <Sparkles className="size-3.5" />, submenu: projects.length ? projects : [{ label: "None found", disabled: true, onClick: () => {} }] } as MenuItem]
        : []),
    ];
    const others = recent.filter((r) => r !== cwd);
    if (others.length) {
      items.push("separator");
      for (const r of others) items.push({ label: tildify(r), icon: isRemote(r) ? <Server className="size-3.5" /> : <FolderOpen className="size-3.5" />, onClick: () => setCwd(r) });
      items.push({ label: "Clear recent folders", icon: <X className="size-3.5" />, onClick: () => others.forEach(forgetRecent) });
    }
    if (cwd) {
      items.push(
        "separator",
        ...(branch && gitRoot && !isRemote(gitRoot) ? [{ label: "Open Git repository", icon: <FolderGit2 className="size-3.5" />, onClick: () => openGitRepo(gitRoot) }] : []),
        { label: "Copy folder path", icon: <Copy className="size-3.5" />, onClick: () => void copyText(remoteParts(cwd)?.path ?? cwd) },
        ...(isRemote(cwd) ? [] : [{ label: REVEAL_LABEL, icon: <FolderSearch className="size-3.5" />, onClick: () => void revealPath(cwd) } as MenuItem]),
        { label: "Close folder", icon: <X className="size-3.5" />, onClick: () => setCwd(null) },
      );
    }
    menu.show(at, items);
  };

  return (
    <div className="shrink-0 border-b border-line px-2 py-1.5">
      <button
        className="flex w-full items-center gap-1.5 rounded px-1.5 py-1 text-left hover:bg-hover"
        title={cwd ?? "Open a folder"}
        onClick={(e) => void showMenu(e)}
      >
        {isRemote(cwd) ? <Server className="size-4 shrink-0 text-accent" /> : <FolderOpen className={clsx("size-4 shrink-0", cwd ? "text-accent" : "text-faint")} />}
        <div className="min-w-0 flex-1">
          <div className={clsx("truncate text-[13px] font-medium", !cwd && "text-muted")}>{cwd ? basename(cwd) : "Open folder…"}</div>
          {cwd && <div className="truncate text-[11px] text-faint">{tildify(cwd)}</div>}
        </div>
        {branch && (
          <span className="inline-flex max-w-[45%] shrink-0 items-center gap-0.5 rounded bg-hover px-1.5 py-px font-mono text-[11px] text-muted" title={`Branch ${branch}`}>
            <GitBranch className="size-3 shrink-0" /><span className="truncate">{branch}</span>
          </span>
        )}
        <ChevronDown className="size-3.5 shrink-0 text-faint" />
      </button>
      {cwd && (
        <div className="mt-1.5 flex gap-1.5">
          <Button variant="primary" className="min-w-0 flex-1 justify-center" onClick={() => openClaudeWorkspace(cwd)}>
            <Plus className="size-3.5" /> New {isClaude ? "" : `${provider.name} `}chat
          </Button>
          <Button className="shrink-0" title="Open a terminal in this folder, in its own tab" onClick={() => openTerminal(cwd)}>
            <SquareTerminal className="size-3.5" /> Terminal
          </Button>
        </div>
      )}
      {menu.element}
      <RemoteFolderDialog />
    </div>
  );
}

/** Branch name for the header — read from the shared git status store below. */
function useGitBranch(cwd: string | null) {
  const git = useSharedGit(cwd);
  return git?.isRepo ? (git.branch ?? "detached") : null;
}

// Git status is needed by both the header (branch) and the tree (coloring); keep a tiny shared cache.
const gitListeners = new Set<() => void>();
let gitCache: { cwd: string; status: GitStatus | null } | null = null;
async function refreshGit(cwd: string) {
  try {
    const status = await api.gitStatus(cwd);
    gitCache = { cwd, status };
  } catch {
    gitCache = { cwd, status: null };
  }
  gitListeners.forEach((l) => l());
}
function useSharedGit(cwd: string | null): GitStatus | null {
  const [, force] = useState(0);
  useEffect(() => {
    const l = () => force((n) => n + 1);
    gitListeners.add(l);
    return () => void gitListeners.delete(l);
  }, []);
  useEffect(() => {
    if (cwd && gitCache?.cwd !== cwd) void refreshGit(cwd);
  }, [cwd]);
  return cwd && gitCache?.cwd === cwd ? gitCache.status : null;
}

function SectionHeader({ title, open, onToggle, count, children }: {
  title: string; open: boolean; onToggle: () => void; count?: number; children?: ReactNode;
}) {
  return (
    <div className="group flex h-7 shrink-0 items-center gap-1 pr-1.5 pl-1">
      <button className="flex min-w-0 flex-1 items-center gap-1 text-[11px] font-semibold tracking-wider text-muted uppercase hover:text-fg" onClick={onToggle}>
        <ChevronRight className={clsx("size-3 transition-transform duration-100", open && "rotate-90")} />
        <span className="truncate">{title}</span>
        {count != null && <span className="font-normal text-faint normal-case">{count}</span>}
      </button>
      {children}
    </div>
  );
}

/** DevCat Agent chats saved for the folder (newest first); click to continue one. */
function AgentSessions({ cwd, providerId, now }: { cwd: string; providerId: string; now: number }) {
  const [list, setList] = useState<{ id: string; title: string; updatedMs: number; messages: number }[] | null>(null);
  const [tick, setTick] = useState(0);
  const busy = useAgent((s) => Object.values(s.chats).some((c) => c.busy));
  // reload when a chat finishes a turn (new or retitled sessions)
  useEffect(() => {
    let live = true;
    api.agentSessions(cwd).then((l) => live && setList(l), () => live && setList([]));
    return () => { live = false; };
  }, [cwd, tick, busy]);
  useEffect(() => { const t = setInterval(() => setTick((n) => n + 1), 30_000); return () => clearInterval(t); }, []);
  // right-click → Rename…: the name turns into a field (Enter / click away saves, Esc cancels)
  const menu = useContextMenu();
  const [editing, setEditing] = useState<string | null>(null);
  const cancelled = useRef(false);
  const rename = async (id: string, value: string, old: string) => {
    setEditing(null);
    const name = value.trim();
    if (cancelled.current || !name || name === old) return;
    try {
      await renameChat(id, name);
      setList((l) => l && l.map((x) => (x.id === id ? { ...x, title: name.split(/\s+/).join(" ") } : x)));
    } catch (e) {
      toast.error(String(e));
    }
  };
  if (!list) return <div className="flex justify-center py-4"><Spinner className="size-4" /></div>;
  if (!list.length) return <div className="px-3 py-2 text-[12px] text-faint">No DevCat Agent chats in this folder yet. Start one with New chat.</div>;
  const open = (s: { id: string; title: string }) => openClaudeWorkspace(cwd, { providerId, sessionId: s.id, title: s.title });
  return (
    <div className="h-full overflow-y-auto pb-2">
      {list.map((s) => editing === s.id ? (
        <div key={s.id} className="px-2 py-1">
          <input
            autoFocus
            defaultValue={s.title}
            aria-label="Chat name"
            spellCheck={false}
            onFocus={(e) => e.currentTarget.select()}
            onKeyDown={(e) => {
              if (e.key === "Enter") e.currentTarget.blur();
              else if (e.key === "Escape") { cancelled.current = true; e.currentTarget.blur(); }
            }}
            onBlur={(e) => void rename(s.id, e.currentTarget.value, s.title)}
            className="h-6.5 w-full rounded border border-accent bg-app px-1.5 text-[13px] text-fg focus:outline-none"
          />
        </div>
      ) : (
        <button key={s.id} className="flex w-full flex-col items-start gap-0.5 px-3 py-1.5 text-left hover:bg-hover"
          onClick={() => open(s)}
          onContextMenu={(e) => menu.show(e, [
            { label: "Open", icon: <MessageSquare className="size-3.5" />, onClick: () => open(s) },
            { label: "Rename…", icon: <Pencil className="size-3.5" />, onClick: () => { cancelled.current = false; setEditing(s.id); } },
          ])}>
          <span className="w-full truncate text-[13px] text-fg">{s.title}</span>
          <span className="text-[11px] text-faint">{relTime(new Date(s.updatedMs).toISOString(), now)} · {s.messages} message{s.messages === 1 ? "" : "s"}</span>
        </button>
      ))}
      {menu.element}
    </div>
  );
}

function Workspace({ cwd }: { cwd: string }) {
  const sessionsOpen = useWorkspace((s) => s.sessionsOpen);
  const setSessionsOpen = useWorkspace((s) => s.setSessionsOpen);
  const git = useSharedGit(cwd);
  // Sessions come from ~/.claude/projects: only shown while Claude Code is the selected AI connection.
  const selected = useSelectedProvider();
  const isClaude = selected.kind === "claude";
  const isAgent = selected.kind === "agent";
  const [sessions, setSessions] = useState<SessionMeta[] | null>(null);
  const [sessErr, setSessErr] = useState<string | null>(null);
  const [sessLoading, setSessLoading] = useState(false);
  const [query, setQuery] = useState("");
  const [now, setNow] = useState(Date.now());
  const [filesOpen, setFilesOpen] = useState(true);
  const tree = useRef<FileTreeHandle>(null);
  const sessPanel = usePanelRef();
  const filesPanel = usePanelRef();

  const loadSessions = useCallback(async (manual = false) => {
    if (!isClaude) return;
    setSessLoading(true);
    try {
      setSessions(await api.claudeSessions(cwd));
      setSessErr(null);
    } catch (e) {
      setSessErr(String(e));
      if (manual) toast.error(`Couldn't load sessions: ${String(e)}`);
    } finally {
      setSessLoading(false);
      setNow(Date.now());
    }
  }, [cwd, isClaude]);

  useEffect(() => { void loadSessions(); }, [loadSessions]);
  useVisibleInterval(() => {
    void loadSessions();
    void refreshGit(cwd);
    tree.current?.refresh();
  }, REFRESH_MS);

  const toggleSessions = () => {
    const p = sessPanel.current;
    if (!p) return;
    if (p.isCollapsed()) p.expand(); else p.collapse();
  };
  const toggleFiles = () => {
    const p = filesPanel.current;
    if (!p) return;
    if (p.isCollapsed()) p.expand(); else p.collapse();
  };

  // Restore the persisted collapsed state once panels exist.
  useEffect(() => {
    if (!sessionsOpen) sessPanel.current?.collapse();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <Group orientation="vertical" className="min-h-0 flex-1">
      <Panel
        panelRef={sessPanel}
        defaultSize="38%"
        minSize="90px"
        collapsible
        collapsedSize="28px"
        onResize={() => {
          const open = !(sessPanel.current?.isCollapsed() ?? false);
          if (open !== sessionsOpen) setSessionsOpen(open);
        }}
      >
        <div className="flex h-full flex-col">
          <SectionHeader title="Sessions" open={sessionsOpen} onToggle={toggleSessions} count={isClaude ? sessions?.length : undefined}>
            <IconButton title="Refresh sessions" className="size-5.5" onClick={() => { void loadSessions(true); void refreshGit(cwd); }}>
              <RefreshCw className={clsx("size-3.5", sessLoading && "animate-spin")} />
            </IconButton>
          </SectionHeader>
          {sessionsOpen && isAgent && <AgentJobs cwd={cwd} providerId={selected.id} />}
          {sessionsOpen && isAgent && <div className="min-h-0 flex-1"><AgentSessions cwd={cwd} providerId={selected.id} now={now} /></div>}
          {sessionsOpen && !isClaude && !isAgent && (
            <div className="px-3 py-2 text-[12px] text-faint">Sessions are available for Claude Code and DevCat Agent. Pick one in the header above to browse and resume them.</div>
          )}
          {sessionsOpen && isClaude && (
            <div className="min-h-0 flex-1">
              <SessionsList cwd={cwd} sessions={sessions} error={sessErr} loading={sessLoading} query={query} setQuery={setQuery} now={now} />
            </div>
          )}
        </div>
      </Panel>
      <Separator className="h-px bg-line hover:bg-accent data-[separator=active]:bg-accent" />
      <Panel
        panelRef={filesPanel}
        minSize="80px"
        collapsible
        collapsedSize="28px"
        onResize={() => setFilesOpen(!(filesPanel.current?.isCollapsed() ?? false))}
      >
        <div className="flex h-full flex-col">
          <SectionHeader title="Files" open={filesOpen} onToggle={toggleFiles} />
          <div className={clsx("min-h-0 flex-1", !filesOpen && "hidden")}>
            <FileTree ref={tree} cwd={cwd} git={git} />
          </div>
        </div>
      </Panel>
    </Group>
  );
}
