// Diff view: changed files (Staged / Changes / Untracked) + Monaco diff of the selected file,
// stage / unstage / discard, commit, and "Ask <agent> to review". `ChangesPanel` is also used by the Git tab.
import { DiffEditor, type DiffOnMount } from "../../components/DiffEditor";
import clsx from "clsx";
import { Columns2, ExternalLink, FileDiff, FolderGit2, GitCommitHorizontal, ListChecks, Minus, Plus, RefreshCw, Rows2, ScanSearch, Trash2, Undo2, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { toast } from "sonner";
import { Button, Checkbox, Dialog, Empty, ErrorBox, IconButton, Select, Spinner, Textarea, Spin } from "../../components/ui";
import { api, errMsg } from "../../lib/api";
import type { FileVersions, GitFile, GitStatus } from "../../lib/types";
import { refreshGit, setRepo, useClaude } from "./store";
import { basename, languageOf, openFile } from "./util";

type Group = "staged" | "changes" | "untracked";
type Sel = { path: string; group: Group };

const STATUS_COLOR: Record<string, string> = {
  M: "text-warn", A: "text-ok", D: "text-danger", R: "text-info", C: "text-info", U: "text-danger", "?": "text-ok", T: "text-warn",
};

function groupFiles(files: GitFile[]) {
  const staged: GitFile[] = [], changes: GitFile[] = [], untracked: GitFile[] = [];
  for (const f of files) {
    if (f.index === "?" || f.worktree === "?") { untracked.push(f); continue; }
    if (f.index && f.index !== " " && f.index !== ".") staged.push(f);
    if (f.worktree && f.worktree !== " " && f.worktree !== ".") changes.push(f);
  }
  return { staged, changes, untracked };
}

/** Tri-state checkbox (checked / some / none). */
function Check({ state, onChange, title }: { state: boolean | "some"; onChange: (v: boolean) => void; title: string }) {
  return (
    <input
      type="checkbox"
      title={title}
      className="size-3 shrink-0 cursor-pointer accent-[var(--color-accent)]"
      checked={state === true}
      ref={(el) => { if (el) el.indeterminate = state === "some"; }}
      onClick={(e) => e.stopPropagation()}
      onChange={(e) => onChange(e.target.checked)}
    />
  );
}

function FileRow({ f, group, selected, checked, onCheck, onSelect, onAction }: {
  f: GitFile; group: Group; selected: boolean; checked: boolean; onCheck: (v: boolean) => void;
  onSelect: () => void; onAction: (a: "stage" | "unstage" | "discard") => void;
}) {
  const letter = group === "staged" ? f.index : group === "untracked" ? "U" : f.worktree;
  const dir = f.path.includes("/") ? f.path.slice(0, f.path.lastIndexOf("/")) : "";
  return (
    <div
      className={clsx("group flex h-6 cursor-pointer items-center gap-1.5 pl-2 pr-1 text-xs", selected ? "bg-accent/15 text-fg" : "text-fg/85 hover:bg-hover")}
      onClick={onSelect}
      title={f.oldPath ? `${f.oldPath} → ${f.path}` : f.path}
    >
      <Check state={checked} onChange={onCheck} title="Select for commit / discard" />
      <span className="truncate">{basename(f.path)}</span>
      <span className="min-w-0 flex-1 truncate text-[11px] text-faint">{dir}</span>
      <span className="hidden shrink-0 items-center group-hover:flex" onClick={(e) => e.stopPropagation()}>
        {group !== "staged" && <IconButton title="Discard changes" className="size-5" onClick={() => onAction("discard")}><Undo2 className="size-3" /></IconButton>}
        {group === "staged"
          ? <IconButton title="Unstage" className="size-5" onClick={() => onAction("unstage")}><Minus className="size-3" /></IconButton>
          : <IconButton title="Stage" className="size-5" onClick={() => onAction("stage")}><Plus className="size-3" /></IconButton>}
      </span>
      <span className={clsx("w-3 shrink-0 text-center font-mono text-[11px] font-semibold", STATUS_COLOR[letter] ?? "text-muted")}>{letter}</span>
    </div>
  );
}

/** Git repositories directly inside `cwd` (for folders that hold several projects). */
function useNestedRepos(cwd: string): string[] {
  const [repos, setRepos] = useState<string[]>([]);
  useEffect(() => {
    let live = true;
    api.fsList(cwd).then((l) => live && setRepos(l.filter((e) => e.isDir && e.isRepo).map((e) => e.path)), () => live && setRepos([]));
    return () => { live = false; };
  }, [cwd]);
  return repos;
}

export function DiffView({ tabId, cwd, active, onReview, agent }: { tabId: string; cwd: string; active: boolean; onReview: () => void; agent?: string }) {
  const repo = useClaude((s) => s.tabs[tabId]?.repo ?? null);
  const nested = useNestedRepos(cwd);
  const git = useClaude((s) => s.tabs[tabId]?.git);
  const gitError = useClaude((s) => s.tabs[tabId]?.gitError);
  const gitLoading = useClaude((s) => !!s.tabs[tabId]?.gitLoading);
  const gitTick = useClaude((s) => s.tabs[tabId]?.gitTick ?? 0);
  const focus = useClaude((s) => s.tabs[tabId]?.diffFocus);
  const refresh = useCallback(() => refreshGit(tabId), [tabId]);

  // The tab header polls git every 5s while visible; refresh right away when shown.
  useEffect(() => {
    if (active) void refreshGit(tabId);
  }, [active, tabId]);

  // The folder isn't a repository but holds some: let the user pick one.
  if (!repo && git && !git.isRepo && nested.length) {
    return (
      <Empty icon={<FolderGit2 className="size-8" />} title={`${basename(cwd)} holds ${nested.length} git ${nested.length === 1 ? "repository" : "repositories"}`}>
        <p className="mb-2">Pick one to see its changes:</p>
        <div className="flex w-72 flex-col gap-1">
          {nested.map((r) => (
            <button key={r} onClick={() => setRepo(tabId, r)} className="flex items-center gap-2 rounded-md border border-line px-3 py-2 text-left text-[13px] text-fg hover:border-accent/50 hover:bg-hover">
              <FolderGit2 className="size-4 shrink-0 text-accent" /><span className="truncate">{basename(r)}</span>
            </button>
          ))}
        </div>
      </Empty>
    );
  }
  return (
    <div className="flex h-full flex-col">
      {repo && (
        <div className="flex h-8 shrink-0 items-center gap-2 border-b border-line bg-panel px-3 text-[12px]">
          <FolderGit2 className="size-3.5 text-muted" />
          <span className="text-muted">Repository</span>
          <Select className="h-6 w-56 text-xs" value={repo} onChange={(e) => setRepo(tabId, e.target.value || null)}>
            {nested.map((r) => <option key={r} value={r}>{basename(r)}</option>)}
            {!nested.includes(repo) && <option value={repo}>{basename(repo)}</option>}
            <option value="">{basename(cwd)} (the workspace folder)</option>
          </Select>
        </div>
      )}
      <div className="min-h-0 flex-1">
        <ChangesPanel cwd={repo ?? cwd} git={git ?? null} gitError={gitError ?? null} gitLoading={gitLoading} gitTick={gitTick}
          focus={focus ?? null} active={active} refresh={refresh} onReview={onReview} agent={agent} />
      </div>
    </div>
  );
}

/** Working-tree changes of the repo at `cwd`, independent of where the git status comes from
 *  (the Claude tab store, or the Git tab). `refresh` must re-read the status. */
export function ChangesPanel({ cwd, git, gitError, gitLoading, gitTick, focus, active, refresh, onReview, agent = "Claude", actions }: {
  cwd: string;
  git: GitStatus | null;
  gitError: string | null;
  gitLoading: boolean;
  /** Bumped after every status poll; the open file is re-read on it. */
  gitTick: number;
  focus?: { path: string; line?: number | null; seq: number } | null;
  active: boolean;
  refresh: () => Promise<void> | void;
  onReview?: () => void;
  /** Name in "Ask … to review" (the AI connection the review goes to). */
  agent?: string;
  /** Extra buttons on the right of the file list toolbar. */
  actions?: ReactNode;
}) {
  const [sel, setSel] = useState<Sel | null>(null);
  const [versions, setVersions] = useState<FileVersions | null>(null);
  const [vErr, setVErr] = useState<string | null>(null);
  const [vLoading, setVLoading] = useState(false);
  const [sideBySide, setSideBySide] = useState(true);
  const [msg, setMsg] = useState("");
  const [committing, setCommitting] = useState(false);
  const [confirm, setConfirm] = useState<{
    title: string; body: string; ok?: string;
    /** Optional extra choice shown as a checkbox; its value is passed to run. */
    option?: { label: string; initial: boolean };
    run: (option: boolean) => Promise<void>;
  } | null>(null);
  const [confirmOpt, setConfirmOpt] = useState(false);
  /** Paths ticked for "commit selected" / bulk actions. */
  const [checked, setChecked] = useState<Set<string>>(() => new Set());
  const editorRef = useRef<Parameters<DiffOnMount>[0] | null>(null);
  const revealLine = useRef<number | null>(null);

  const groups = useMemo(() => groupFiles(git?.files ?? []), [git]);

  // Keep the selection valid; pick the first file by default.
  useEffect(() => {
    if (!git?.files) return;
    const list: Sel[] = [
      ...groups.staged.map((f) => ({ path: f.path, group: "staged" as const })),
      ...groups.changes.map((f) => ({ path: f.path, group: "changes" as const })),
      ...groups.untracked.map((f) => ({ path: f.path, group: "untracked" as const })),
    ];
    if (sel && list.some((x) => x.path === sel.path && x.group === sel.group)) return;
    const samePath = sel && list.find((x) => x.path === sel.path);
    setSel(samePath || list[0] || null);
  }, [git, groups, sel]);

  // Drop ticks for files that no longer have changes.
  useEffect(() => {
    if (!git?.files) return;
    setChecked((c) => {
      if (!c.size) return c;
      const live = new Set(git.files.map((f) => f.path));
      const next = new Set([...c].filter((p) => live.has(p)));
      return next.size === c.size ? c : next;
    });
  }, [git]);
  useEffect(() => { setChecked(new Set()); }, [cwd]);

  // Focus requests from the review view.
  useEffect(() => {
    if (!focus || !git?.files) return;
    const root = (git.root ?? cwd).replace(/[\\/]+$/, "");
    const rel = focus.path.startsWith(root + "/") ? focus.path.slice(root.length + 1) : focus.path.replace(/^\.\//, "");
    const f = git.files.find((x) => x.path === rel || rel.endsWith("/" + x.path) || x.path.endsWith("/" + rel));
    if (!f) return;
    const g = groupFiles([f]);
    setSel({ path: f.path, group: g.changes.length ? "changes" : g.staged.length ? "staged" : "untracked" });
    revealLine.current = focus.line ?? null;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [focus?.seq, git?.files]);

  // Only the latest request may update the view (an older one may be for another file).
  const loadSeq = useRef(0);
  const loadVersions = useCallback(async (s: Sel | null, quiet = false) => {
    const seq = ++loadSeq.current;
    if (!s) return setVersions(null);
    if (!quiet) setVLoading(true);
    try {
      const v = await api.gitFileVersions(cwd, s.path, s.group === "staged");
      if (seq !== loadSeq.current) return;
      // Same content → keep the object so the editor isn't touched.
      setVersions((cur) => (cur && cur.original === v.original && cur.modified === v.modified && cur.binary === v.binary ? cur : v));
      setVErr(null);
    } catch (e) {
      if (!quiet && seq === loadSeq.current) {
        setVErr(errMsg(e));
        setVersions(null);
      }
    } finally {
      if (!quiet && seq === loadSeq.current) setVLoading(false);
    }
  }, [cwd]);

  // `reload` re-reads the current selection after an action (which may also change the selection).
  const [reload, setReload] = useState(0);
  useEffect(() => { void loadVersions(sel); }, [sel, loadVersions, reload]);
  // Re-read the open file on each git poll while visible (the AI agent edits it from the terminal).
  useEffect(() => { if (gitTick && active) void loadVersions(sel, true); /* eslint-disable-next-line react-hooks/exhaustive-deps */ }, [gitTick]);

  const act = async (fn: () => Promise<unknown>, okMsg?: string) => {
    try {
      await fn();
      if (okMsg) toast.success(okMsg);
    } catch (e) {
      toast.error(errMsg(e));
    }
    await refresh();
    setReload((n) => n + 1);
  };

  const onAction = (f: GitFile, group: Group, a: "stage" | "unstage" | "discard") => {
    if (a === "stage") return void act(() => api.gitStage(cwd, [f.path]));
    if (a === "unstage") return void act(() => api.gitUnstage(cwd, [f.path]));
    setConfirm({
      title: "Discard changes?",
      body: group === "untracked" ? `Delete untracked file ${f.path}? This cannot be undone.` : `Discard all unstaged changes in ${f.path}? This cannot be undone.`,
      run: () => act(() => api.gitDiscard(cwd, [f.path])),
    });
  };

  const setCheck = (paths: string[], on: boolean) => setChecked((c) => {
    const n = new Set(c);
    for (const p of paths) if (on) n.add(p); else n.delete(p);
    return n;
  });
  const checkedFiles = (git?.files ?? []).filter((f) => checked.has(f.path));
  const isUntracked = (f: GitFile) => f.index === "?" || f.worktree === "?";

  const askConfirm = (c: NonNullable<typeof confirm>) => { setConfirmOpt(c.option?.initial ?? false); setConfirm(c); };

  const discardSelected = () => {
    const paths = checkedFiles.map((f) => f.path);
    if (!paths.length) return;
    const untracked = checkedFiles.filter(isUntracked).length;
    askConfirm({
      title: `Discard ${paths.length} file${paths.length === 1 ? "" : "s"}?`,
      body: `All changes to the selected files, staged and unstaged, go back to the last commit.${untracked ? ` ${untracked} untracked file${untracked === 1 ? " is" : "s are"} deleted.` : ""} This cannot be undone.`,
      run: () => act(async () => { await api.gitDiscard(cwd, paths, true); setChecked(new Set()); }, `Discarded ${paths.length} file${paths.length === 1 ? "" : "s"}`),
    });
  };

  const discardAll = () => {
    const files = git?.files ?? [];
    const untracked = files.filter(isUntracked).length;
    const tracked = files.length - untracked;
    askConfirm({
      title: "Discard all changes?",
      body: `Every staged and unstaged change${tracked ? ` in ${tracked} file${tracked === 1 ? "" : "s"}` : ""} goes back to the last commit. This cannot be undone.`,
      ok: "Discard all",
      option: untracked ? { label: `Also delete ${untracked} untracked file${untracked === 1 ? "" : "s"} / folder${untracked === 1 ? "" : "s"}`, initial: true } : undefined,
      run: (withUntracked) => act(async () => { await api.gitDiscardAll(cwd, withUntracked); setChecked(new Set()); }, "Discarded all changes"),
    });
  };

  const commit = async () => {
    if (!msg.trim()) return;
    const only = checked.size ? [...checked] : null;
    if (!only && !groups.staged.length) return;
    setCommitting(true);
    try {
      const out = await api.gitCommit(cwd, msg.trim(), only);
      toast.success(`${out?.split("\n")[0] || "Committed"}${only ? ` · ${only.length} file${only.length === 1 ? "" : "s"}` : ""}`);
      setMsg("");
      setChecked(new Set());
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setCommitting(false);
      void refresh();
    }
  };

  const onMount: DiffOnMount = (ed) => {
    editorRef.current = ed;
  };
  useEffect(() => {
    const line = revealLine.current;
    if (!line || !versions || !editorRef.current) return;
    revealLine.current = null;
    const m = editorRef.current.getModifiedEditor();
    setTimeout(() => {
      m.revealLineInCenter(line);
      m.setPosition({ lineNumber: line, column: 1 });
    }, 50);
  }, [versions]);

  if (!git && gitError) return <ErrorBox error={gitError} />;
  if (!git) return <div className="flex h-full items-center justify-center"><Spinner /></div>;
  if (!git.isRepo) {
    return (
      <Empty icon={<FileDiff className="size-8" />} title="Not a git repository">
        {basename(cwd)} isn't tracked by git, so there is nothing to diff. Run <code className="text-accent">git init</code> in the folder to track changes.
      </Empty>
    );
  }

  const section = (title: string, list: GitFile[], group: Group) =>
    list.length > 0 && (
      <div className="pb-1">
        <div className="flex h-6 items-center gap-1.5 px-2 text-[11px] font-semibold uppercase tracking-wide text-muted">
          <Check
            state={list.every((f) => checked.has(f.path)) ? true : list.some((f) => checked.has(f.path)) ? "some" : false}
            onChange={(v) => setCheck(list.map((f) => f.path), v)}
            title={`Select all ${title.toLowerCase()}`}
          />
          <span className="flex-1">{title}</span>
          <span className="font-normal text-faint">{list.length}</span>
          {group === "staged"
            ? <IconButton title="Unstage all" className="size-5" onClick={() => void act(() => api.gitUnstage(cwd, list.map((f) => f.path)))}><Minus className="size-3" /></IconButton>
            : <IconButton title="Stage all" className="size-5" onClick={() => void act(() => api.gitStage(cwd, list.map((f) => f.path)))}><Plus className="size-3" /></IconButton>}
        </div>
        {list.map((f) => (
          <FileRow key={group + f.path} f={f} group={group} selected={sel?.path === f.path && sel.group === group}
            checked={checked.has(f.path)} onCheck={(v) => setCheck([f.path], v)}
            onSelect={() => setSel({ path: f.path, group })} onAction={(a) => onAction(f, group, a)} />
        ))}
      </div>
    );

  const unstaged = [...groups.changes, ...groups.untracked];
  const root = git.root ?? cwd;
  return (
    <div className="flex h-full min-h-0">
      <div className="flex w-72 shrink-0 flex-col border-r border-line bg-panel">
        <div className="flex h-8 shrink-0 items-center gap-1 border-b border-line px-2">
          <IconButton title="Refresh" onClick={() => void refresh()}><Spin icon={RefreshCw} spinning={gitLoading} className="size-3.5" /></IconButton>
          <IconButton title="Stage all" disabled={!unstaged.length} onClick={() => void act(() => api.gitStage(cwd, unstaged.map((f) => f.path)))}><Plus className="size-3.5" /></IconButton>
          <IconButton title="Discard all changes…" disabled={!git.files.length} onClick={discardAll}><Undo2 className="size-3.5" /></IconButton>
          <IconButton
            title={checked.size === git.files.length ? "Clear selection" : "Select all files"}
            disabled={!git.files.length}
            onClick={() => setChecked(checked.size === git.files.length ? new Set() : new Set(git.files.map((f) => f.path)))}
          ><ListChecks className="size-3.5" /></IconButton>
          <div className="flex-1" />
          {actions}
          {onReview && <Button size="sm" variant="ghost" onClick={onReview} title={`Ask ${agent} to review these changes`}><ScanSearch className="size-3.5" />Ask {agent} to review</Button>}
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto py-1">
          {git.files.length === 0 && <div className="px-3 py-6 text-center text-xs text-muted">Working tree clean</div>}
          {section("Staged", groups.staged, "staged")}
          {section("Changes", groups.changes, "changes")}
          {section("Untracked", groups.untracked, "untracked")}
        </div>
        {/* Below the list, so ticking the first file doesn't shift the rows. */}
        {checked.size > 0 && (
          <div className="flex h-7 shrink-0 items-center gap-1 border-t border-line bg-accent/10 px-2 text-xs">
            <span className="flex-1 text-fg/90">{checked.size} selected</span>
            <IconButton title="Stage selected" className="size-5" onClick={() => void act(() => api.gitStage(cwd, [...checked]))}><Plus className="size-3" /></IconButton>
            <IconButton title="Unstage selected" className="size-5" onClick={() => void act(() => api.gitUnstage(cwd, [...checked]))}><Minus className="size-3" /></IconButton>
            <IconButton title="Discard selected…" className="size-5 hover:text-danger" onClick={discardSelected}><Trash2 className="size-3" /></IconButton>
            <IconButton title="Clear selection" className="size-5" onClick={() => setChecked(new Set())}><X className="size-3" /></IconButton>
          </div>
        )}
        <div className="shrink-0 border-t border-line p-2">
          <Textarea
            rows={2}
            className="resize-none font-sans text-xs"
            placeholder={checked.size ? `Commit message (${checked.size} selected) — ⌘↵ to commit` : groups.staged.length ? `Commit message (${groups.staged.length} staged) — ⌘↵ to commit` : "Stage or select files to commit"}
            value={msg}
            onChange={(e) => setMsg(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) { e.preventDefault(); void commit(); } }}
          />
          <Button variant="primary" className="mt-1.5 w-full justify-center" loading={committing} disabled={!msg.trim() || (!checked.size && !groups.staged.length)} onClick={() => void commit()}
            title={checked.size ? "Commit only the selected files (other staged changes stay staged)" : "Commit the staged changes"}>
            <GitCommitHorizontal className="size-3.5" />{checked.size ? `Commit ${checked.size} selected` : "Commit"}
          </Button>
        </div>
      </div>
      <div className="flex min-w-0 flex-1 flex-col">
        <div className="flex h-8 shrink-0 items-center gap-2 border-b border-line bg-panel px-3 text-xs">
          {sel ? (
            <>
              <span className="truncate font-mono text-fg/90">{sel.path}</span>
              <span className="text-faint">{sel.group === "staged" ? "HEAD ↔ index" : sel.group === "untracked" ? "new file" : "index ↔ working tree"}</span>
              {vLoading && <Spinner className="size-3" />}
              <div className="flex-1" />
              <IconButton title="Open file" onClick={() => openFile(root, sel.path)}><ExternalLink className="size-3.5" /></IconButton>
              <IconButton title={sideBySide ? "Inline view" : "Side-by-side view"} onClick={() => setSideBySide(!sideBySide)}>
                {sideBySide ? <Rows2 className="size-3.5" /> : <Columns2 className="size-3.5" />}
              </IconButton>
            </>
          ) : <span className="text-faint">No file selected</span>}
        </div>
        <div className="relative min-h-0 flex-1">
          {vErr ? <ErrorBox error={vErr} /> : !sel ? (
            <Empty icon={<FileDiff className="size-8" />} title="No changes">Edits made by the AI agent or by you show up here.</Empty>
          ) : versions?.binary ? (
            <Empty title="Binary file">This file is binary; no text diff available.</Empty>
          ) : versions ? (
            <DiffEditor
              theme="devcat"
              original={versions.original}
              modified={versions.modified}
              language={languageOf(sel.path)}
              onMount={onMount}
              options={{
                readOnly: true, renderSideBySide: sideBySide, minimap: { enabled: false }, fontSize: 12,
                scrollBeyondLastLine: false, automaticLayout: true, renderOverviewRuler: false, originalEditable: false,
                hideUnchangedRegions: { enabled: true },
              }}
            />
          ) : null}
        </div>
      </div>
      <Dialog
        open={!!confirm}
        onClose={() => setConfirm(null)}
        title={confirm?.title}
        width={440}
        footer={
          <>
            <Button variant="ghost" onClick={() => setConfirm(null)}>Cancel</Button>
            <Button variant="danger" onClick={() => { const c = confirm; setConfirm(null); void c?.run(confirmOpt); }}>{confirm?.ok ?? "Discard"}</Button>
          </>
        }
      >
        <div className="text-[13px] text-fg/90">{confirm?.body}</div>
        {confirm?.option && <div className="mt-3"><Checkbox label={confirm.option.label} checked={confirmOpt} onChange={setConfirmOpt} /></div>}
      </Dialog>
    </div>
  );
}
