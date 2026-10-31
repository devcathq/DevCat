// Git tab for one repository: header (branch switcher, fetch / pull / push, in-progress operation)
// and History (graph) / Changes / Branches / Stashes views.
import clsx from "clsx";
import {
  AlertTriangle, Archive, ArrowDown, ArrowDownToLine, ArrowUp, ArrowUpFromLine, Check, ChevronDown, Copy, FolderSearch, GitBranch, GitBranchPlus,
  RefreshCw, Tags,
} from "lucide-react";
import { AiTerminalIcon } from "../../components/ui/AppIcons";
import { useCallback, useEffect, useState, type MouseEvent } from "react";
import { Button, ErrorBox, IconButton, Spin } from "../../components/ui";
import { api } from "../../lib/api";
import { useTabs } from "../../store/tabs";
import type { GitView, Tab } from "../../tabs/types";
import { ChangesPanel } from "../claude/DiffView";
import { openClaudeWorkspace } from "../claude/open";
import { useContextMenu, type MenuItem } from "../connections/ContextMenu";
import { basename, copyText, revealPath, REVEAL_LABEL } from "../files/format";
import { checkout, createBranch, pushCurrent } from "./actions";
import { BranchesView } from "./BranchesView";
import { useGitDialogs, type Dialogs } from "./dialogs";
import { HistoryView } from "./HistoryView";
import { stashChanges, StashesView } from "./StashesView";
import { gitAction, refreshRepo, useRepo } from "./store";
import { operationLabel, tildify } from "./util";

type GitTabT = Extract<Tab, { type: "git" }>;

const VIEWS: Array<{ id: GitView; label: string }> = [
  { id: "history", label: "History" },
  { id: "changes", label: "Changes" },
  { id: "branches", label: "Branches" },
  { id: "stashes", label: "Stashes" },
];

/** Refs + status poll interval while the tab is visible. */
const POLL_MS = 8_000;
/** Branches listed in the switcher menu (most recent first). */
const SWITCHER_MAX = 20;

export default function GitTab({ tab }: { tab: GitTabT }) {
  const repo = tab.repo;
  const view = tab.view ?? "history";
  const active = useTabs((s) => s.activeId === tab.id);
  const { refs, refsError, status, loading } = useRepo(repo);
  const { dlg, element } = useGitDialogs();
  // Views mount on first visit and then stay mounted (keeps scroll / selection).
  const [visited, setVisited] = useState<Set<GitView>>(() => new Set([view]));
  useEffect(() => { setVisited((v) => (v.has(view) ? v : new Set(v).add(view))); }, [view]);

  const setView = useCallback((v: GitView) => useTabs.getState().update(tab.id, { view: v } as never), [tab.id]);
  const showInHistory = useCallback((sha: string) => useTabs.getState().update(tab.id, { view: "history", sha } as never), [tab.id]);

  // Refresh when shown, then poll while visible.
  useEffect(() => {
    if (!active) return;
    void refreshRepo(repo);
    const t = setInterval(() => { if (!document.hidden) void refreshRepo(repo); }, POLL_MS);
    const onFocus = () => void refreshRepo(repo);
    window.addEventListener("focus", onFocus);
    return () => { clearInterval(t); window.removeEventListener("focus", onFocus); };
  }, [active, repo]);

  const changes = status?.isRepo ? status.files.length : 0;

  if (!refs && refsError && !status?.isRepo) {
    return (
      <div className="flex h-full flex-col bg-app">
        <ErrorBox error={`${repo}\n\n${refsError}`} />
        <div className="px-3"><Button onClick={() => void refreshRepo(repo)}><RefreshCw className="size-3.5" />Retry</Button></div>
      </div>
    );
  }

  return (
    <div className="flex h-full min-h-0 flex-col bg-app">
      <div className="flex h-10 shrink-0 items-center gap-2 border-b border-line bg-panel px-3">
        <div className="flex min-w-0 max-w-56 flex-col leading-tight">
          <span className="truncate text-[13px] font-semibold">{basename(repo)}</span>
          <span className="truncate text-[10px] text-faint" title={repo}>{tildify(repo)}</span>
        </div>
        <div className="ml-1 flex h-7 shrink-0 items-center rounded-md border border-line bg-app p-0.5">
          {VIEWS.map((v) => (
            <button
              key={v.id}
              type="button"
              onClick={() => setView(v.id)}
              className={clsx("flex h-6 items-center gap-1.5 rounded px-2.5 text-xs", view === v.id ? "bg-raised text-fg shadow-sm" : "text-muted hover:text-fg")}
            >
              {v.label}
              {v.id === "changes" && changes > 0 && <span className="rounded bg-warn/15 px-1 font-mono text-[10px] text-warn">{changes}</span>}
            </button>
          ))}
        </div>
        <BranchSwitcher repo={repo} dlg={dlg} onMore={() => setView("branches")} />
        <div className="flex-1" />
        <RemoteButtons repo={repo} dlg={dlg} />
        <div className="mx-0.5 h-4 w-px shrink-0 bg-line" />
        <RepoMenu repo={repo} />
        <IconButton title="Refresh" onClick={() => void refreshRepo(repo)}><Spin icon={RefreshCw} spinning={loading} className="size-3.5" /></IconButton>
      </div>
      {refs?.operation && <OperationBanner repo={repo} op={refs.operation} dlg={dlg} onResolve={() => setView("changes")} />}
      <div className="relative min-h-0 flex-1">
        {visited.has("history") && (
          <div className={clsx("absolute inset-0", view !== "history" && "hidden")}>
            <HistoryView tabId={tab.id} repo={repo} path={tab.path ?? null} target={tab.sha ?? null} dlg={dlg} />
          </div>
        )}
        {visited.has("changes") && (
          <div className={clsx("absolute inset-0", view !== "changes" && "hidden")}>
            <ChangesView repo={repo} dlg={dlg} active={active && view === "changes"} />
          </div>
        )}
        {visited.has("branches") && (
          <div className={clsx("absolute inset-0", view !== "branches" && "hidden")}>
            <BranchesView repo={repo} dlg={dlg} showInHistory={showInHistory} />
          </div>
        )}
        {visited.has("stashes") && (
          <div className={clsx("absolute inset-0", view !== "stashes" && "hidden")}>
            <StashesView repo={repo} dlg={dlg} active={active && view === "stashes"} />
          </div>
        )}
      </div>
      {element}
    </div>
  );
}

/** Working-tree changes: the Claude tab's diff view, fed from this repo's status. */
function ChangesView({ repo, dlg, active }: { repo: string; dlg: Dialogs; active: boolean }) {
  const { status, statusError, loading, tick } = useRepo(repo);
  const refresh = useCallback(() => refreshRepo(repo), [repo]);
  const dirty = !!status?.files.length;
  return (
    <ChangesPanel
      cwd={repo}
      git={status}
      gitError={statusError}
      gitLoading={loading}
      gitTick={tick}
      active={active}
      refresh={refresh}
      actions={
        <Button size="sm" variant="ghost" disabled={!dirty} onClick={() => void stashChanges(repo, dlg)} title="Stash the working-tree changes">
          <Archive className="size-3.5" />Stash…
        </Button>
      }
    />
  );
}

function BranchSwitcher({ repo, dlg, onMore }: { repo: string; dlg: Dialogs; onMore: () => void }) {
  const { refs } = useRepo(repo);
  const menu = useContextMenu();
  if (!refs) return null;
  const cur = refs.local.find((b) => b.current);
  const label = refs.current ?? (refs.headSha ? `detached at ${refs.headSha.slice(0, 7)}` : "no commits");

  const open = (e: MouseEvent) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const local = [...refs.local].sort((a, b) => Number(b.current) - Number(a.current) || b.time - a.time);
    const items: MenuItem[] = [
      { label: "New branch…", icon: <GitBranchPlus className="size-3.5" />, onClick: () => void createBranch(repo, dlg, null, refs.current ?? "HEAD") },
      "separator",
      ...local.slice(0, SWITCHER_MAX).map((b): MenuItem => ({
        label: b.name,
        icon: b.current ? <Check className="size-3.5 text-accent" /> : <GitBranch className="size-3.5" />,
        disabled: b.current,
        onClick: () => void checkout(repo, b.name),
      })),
    ];
    const remotes = refs.remote.filter((b) => !b.name.endsWith("/HEAD") && !refs.local.some((l) => l.upstream === b.name));
    if (remotes.length) {
      items.push({
        label: "Remote branches",
        icon: <GitBranch className="size-3.5" />,
        submenu: remotes.sort((a, b) => b.time - a.time).slice(0, SWITCHER_MAX).map((b): MenuItem => ({ label: b.name, onClick: () => void checkout(repo, b.name) })),
      });
    }
    if (local.length > SWITCHER_MAX) items.push({ label: `${local.length - SWITCHER_MAX} more in Branches…`, onClick: onMore });
    menu.show({ clientX: r.left, clientY: r.bottom + 2 }, items);
  };

  return (
    <>
      <button
        type="button"
        onClick={open}
        title={cur?.upstream ? `${label} → ${cur.upstream}` : label}
        className="inline-flex h-7 min-w-0 items-center gap-1.5 rounded-md border border-line bg-raised px-2 text-[12px] hover:bg-hover"
      >
        <GitBranch className="size-3.5 shrink-0 text-muted" />
        <span className={clsx("max-w-48 truncate", refs.current ? "text-fg/90" : "text-warn")}>{label}</span>
        {cur && cur.ahead > 0 && <span className="inline-flex items-center text-[11px] text-info"><ArrowUp className="size-3" />{cur.ahead}</span>}
        {cur && cur.behind > 0 && <span className="inline-flex items-center text-[11px] text-warn"><ArrowDown className="size-3" />{cur.behind}</span>}
        <ChevronDown className="size-3 shrink-0 text-faint" />
      </button>
      {menu.element}
    </>
  );
}

function RemoteButtons({ repo, dlg }: { repo: string; dlg: Dialogs }) {
  const { refs } = useRepo(repo);
  const [busy, setBusy] = useState<"fetch" | "pull" | "push" | null>(null);
  const menu = useContextMenu();
  const cur = refs?.local.find((b) => b.current);
  const noRemote = !!refs && refs.remotes.length === 0;

  const run = async (op: "fetch" | "pull" | "push", fn: () => Promise<unknown>) => {
    setBusy(op);
    try { await fn(); } finally { setBusy(null); }
  };
  const doFetch = () => run("fetch", () => gitAction(repo, "Fetch", () => api.gitFetch(repo)));
  const pull = (rebase = false) => run("pull", () => gitAction(repo, rebase ? "Pull (rebase)" : "Pull", () => api.gitPull(repo, { rebase })));
  const push = (o: { force?: boolean; tags?: boolean } = {}) => run("push", () => pushCurrent(repo, dlg, refs, o));

  const more = (e: MouseEvent, which: "pull" | "push") => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const items: MenuItem[] = which === "pull"
      ? [
        { label: "Pull (merge)", icon: <ArrowDownToLine className="size-3.5" />, onClick: () => void pull(false) },
        { label: "Pull (rebase)", icon: <ArrowDownToLine className="size-3.5" />, onClick: () => void pull(true) },
      ]
      : [
        { label: "Push", icon: <ArrowUpFromLine className="size-3.5" />, onClick: () => void push() },
        { label: "Push with tags", icon: <Tags className="size-3.5" />, onClick: () => void push({ tags: true }) },
        "separator",
        { label: "Force push (with lease)…", icon: <AlertTriangle className="size-3.5" />, danger: true, onClick: () => void push({ force: true }) },
      ];
    menu.show({ clientX: r.left, clientY: r.bottom + 2 }, items);
  };

  const pushTitle = !cur ? "Push" : !cur.upstream ? `Publish ${cur.name} (no upstream yet)` : `Push to ${cur.upstream}${cur.ahead ? ` (${cur.ahead} commit${cur.ahead === 1 ? "" : "s"})` : ""}`;
  const split = "rounded-l-none! border-l-0! px-1!";
  return (
    <div className="flex shrink-0 items-center gap-1">
      <Button size="sm" loading={busy === "fetch"} disabled={!!busy || noRemote} onClick={() => void doFetch()} title="Fetch all remotes">
        {busy !== "fetch" && <RefreshCw className="size-3.5" />}Fetch
      </Button>
      <div className="flex">
        <Button size="sm" className="rounded-r-none!" loading={busy === "pull"} disabled={!!busy || noRemote || !cur?.upstream} onClick={() => void pull()}
          title={cur?.upstream ? `Pull from ${cur.upstream}` : "No upstream to pull from"}>
          {busy !== "pull" && <ArrowDownToLine className="size-3.5" />}Pull
          {!!cur?.behind && <span className="font-mono text-[10px] text-warn">{cur.behind}</span>}
        </Button>
        <Button size="sm" className={split} disabled={!!busy || noRemote || !cur?.upstream} onClick={(e) => more(e, "pull")} aria-label="Pull options">
          <ChevronDown className="size-3" />
        </Button>
      </div>
      <div className="flex">
        <Button size="sm" className="rounded-r-none!" loading={busy === "push"} disabled={!!busy || noRemote || !refs?.current} onClick={() => void push()} title={pushTitle}>
          {busy !== "push" && <ArrowUpFromLine className="size-3.5" />}{cur && !cur.upstream ? "Publish" : "Push"}
          {!!cur?.ahead && <span className="font-mono text-[10px] text-info">{cur.ahead}</span>}
        </Button>
        <Button size="sm" className={split} disabled={!!busy || noRemote || !refs?.current} onClick={(e) => more(e, "push")} aria-label="Push options">
          <ChevronDown className="size-3" />
        </Button>
      </div>
      {menu.element}
    </div>
  );
}

function RepoMenu({ repo }: { repo: string }) {
  const menu = useContextMenu();
  const open = (e: MouseEvent) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    menu.show({ clientX: r.left, clientY: r.bottom + 2 }, [
      { label: "New AI chat here", icon: <AiTerminalIcon className="size-3.5" />, onClick: () => openClaudeWorkspace(repo) },
      "separator",
      { label: "Copy path", icon: <Copy className="size-3.5" />, onClick: () => void copyText(repo) },
      { label: REVEAL_LABEL, icon: <FolderSearch className="size-3.5" />, onClick: () => void revealPath(repo) },
    ]);
  };
  return (
    <>
      <IconButton title="More" onClick={open}><ChevronDown className="size-3.5" /></IconButton>
      {menu.element}
    </>
  );
}

function OperationBanner({ repo, op, dlg, onResolve }: { repo: string; op: string; dlg: Dialogs; onResolve: () => void }) {
  const name = operationLabel(op);
  const abort = async () => {
    const ok = await dlg.confirm({
      title: `Abort ${name.toLowerCase()}?`,
      body: <>Stop the {name.toLowerCase()} in progress and return to the state before it started. Conflict resolutions made so far are discarded.</>,
      ok: "Abort",
      danger: true,
    });
    if (ok) await gitAction(repo, `Abort ${name.toLowerCase()}`, () => api.gitAbort(repo));
  };
  return (
    <div className="flex h-8 shrink-0 items-center gap-2 border-b border-warn/30 bg-warn/10 px-3 text-[12px]">
      <AlertTriangle className="size-3.5 shrink-0 text-warn" />
      <span className="text-warn">{name} in progress.</span>
      <span className="min-w-0 truncate text-muted">Resolve conflicts and stage the files, then commit — or abort.</span>
      <div className="flex-1" />
      <Button size="sm" variant="ghost" onClick={onResolve}>Show changes</Button>
      <Button size="sm" variant="danger" onClick={() => void abort()}>Abort</Button>
    </div>
  );
}
