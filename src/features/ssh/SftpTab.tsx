// SFTP tab (Xftp-style; desktop only, see AGENTS.md): this computer's files on the left, the SSH
// host's on the right. Copy with the arrow buttons, the context menu, or by dragging rows from one
// side to the other (files from Finder / Explorer can be dropped on the host's side too).
// Copies run in the backend (src-tauri/src/claude/sftp.rs) and report progress as events.
import { homeDir } from "@tauri-apps/api/path";
import { listen } from "@tauri-apps/api/event";
import clsx from "clsx";
import {
  ArrowLeft, ArrowRight, ArrowUp, Check, Copy, Eye, EyeOff, FolderPlus, Loader2, Monitor, Pencil, RefreshCw, Server, SquareTerminal, Trash2, X,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Group, Panel, Separator } from "react-resizable-panels";
import { toast } from "sonner";
import { IconButton, Input } from "../../components/ui";
import { api, errMsg } from "../../lib/api";
import { remoteHost, remoteParts, remoteUri } from "../../lib/remotePath";
import type { FileEntry } from "../../lib/types";
import { openTab, useTabs } from "../../store/tabs";
import type { Tab } from "../../tabs/types";
import { confirmDialog, promptDialog } from "../connections/dialogs";
import { useContextMenu, type MenuItem } from "../connections/ContextMenu";
import { droppedPaths, isFileDrag } from "../files/drop";
import { FileIcon } from "../files/FileIcon";
import { DRAG_PATHS, basename, copyText, fmtSize, relTime } from "../files/format";
import { openTerminal } from "../terminal/open";

type SftpTabT = Extract<Tab, { type: "sftp" }>;
type Side = "local" | "remote";

interface Job {
  id: string;
  label: string;
  /** Folder the files go to (refreshed when done). */
  dstDir: string;
  file: string;
  files: number;
  filesTotal: number;
  bytes: number;
  bytesTotal: number;
  state: "running" | "done" | "failed" | "cancelled";
  error?: string;
}

const isRemotePath = (p: string) => p.startsWith("ssh://");

/** Parent folder, for local paths and ssh:// URIs alike (null at the top). */
function parentOf(p: string): string | null {
  const r = remoteParts(p);
  const path = r ? r.path : p;
  const trimmed = path.replace(/[\\/]+$/, "");
  const i = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  if (i < 0 || trimmed === "" || /^[A-Za-z]:$/.test(trimmed)) return null;
  const parent = i === 0 ? "/" : trimmed.slice(0, i) + (/^[A-Za-z]:$/.test(trimmed.slice(0, i)) ? "\\" : "");
  return r ? remoteUri(r.dest, r.port, parent) : parent;
}

/** `name` inside folder `dir` (local or ssh://). */
function joinPath(dir: string, name: string): string {
  if (isRemotePath(dir)) return dir.replace(/\/+$/, "") + "/" + name;
  const sep = dir.includes("\\") && !dir.includes("/") ? "\\" : "/";
  return dir.replace(/[\\/]+$/, "") + sep + name;
}

export default function SftpTab({ tab }: { tab: SftpTabT }) {
  const [localDir, setLocalDir] = useState<string | null>(null);
  const [remoteDir, setRemoteDir] = useState<string | null>(null);
  const [remoteError, setRemoteError] = useState<string | null>(null);
  const [showHidden, setShowHidden] = useState(false);
  const [jobs, setJobs] = useState<Job[]>([]);
  const [reload, setReload] = useState({ local: 0, remote: 0 });
  const bump = (side: Side) => setReload((r) => ({ ...r, [side]: r[side] + 1 }));
  const host = remoteHost(tab.cwd);
  /** Each pane's last listing, so the other pane can check for name clashes before copying. */
  const listings = useRef<Record<Side, FileEntry[] | null>>({ local: null, remote: null });

  useEffect(() => { homeDir().then(setLocalDir).catch(() => setLocalDir("/")); }, []);
  // `~` and symlinks resolved on the host, so Up and the path bar show real folders.
  useEffect(() => {
    api.remoteResolveDir(tab.cwd).then(setRemoteDir).catch((e) => setRemoteError(errMsg(e)));
  }, [tab.cwd]);

  // Progress of this tab's copies.
  const jobsRef = useRef(jobs);
  jobsRef.current = jobs;
  useEffect(() => {
    const offs = [
      listen<{ jobId: string; file: string; files: number; filesTotal: number; bytes: number; bytesTotal: number }>("sftp-progress", (e) => {
        const p = e.payload;
        setJobs((js) => js.map((j) => (j.id === p.jobId ? { ...j, file: p.file, files: p.files, filesTotal: p.filesTotal, bytes: p.bytes, bytesTotal: p.bytesTotal } : j)));
      }),
      listen<{ jobId: string; error: string | null; cancelled: boolean }>("sftp-done", (e) => {
        const p = e.payload;
        const job = jobsRef.current.find((j) => j.id === p.jobId);
        if (!job) return;
        setJobs((js) => js.map((j) => (j.id === p.jobId ? { ...j, state: p.cancelled ? "cancelled" : p.error ? "failed" : "done", error: p.error ?? undefined } : j)));
        if (p.error) toast.error(`${job.label} failed: ${p.error}`);
        bump(isRemotePath(job.dstDir) ? "remote" : "local");
      }),
    ];
    return () => {
      offs.forEach((o) => void o.then((f) => f()));
      // The tab was closed: nothing could show or cancel these copies any more.
      for (const j of jobsRef.current) if (j.state === "running") api.sftpCancel(j.id).catch(() => {});
    };
  }, []);

  /** Copy `paths` into the folder `dstDir` (the other side). */
  const transfer = useCallback(async (paths: string[], dstDir: string, existing: FileEntry[] | null) => {
    if (!paths.length) return;
    const names = new Set(existing?.map((e) => e.name));
    const clash = paths.map(basename).filter((n) => names.has(n));
    if (clash.length) {
      const ok = await confirmDialog({
        title: "Replace files?",
        message: <>{clash.length === 1 ? <><b>{clash[0]}</b> already exists</> : <>{clash.length} items already exist</>} in {basename(dstDir) || dstDir}. Replace {clash.length === 1 ? "it" : "them"}? Folders are merged.</>,
        confirmLabel: "Replace",
        danger: true,
      });
      if (!ok) return;
    }
    const up = isRemotePath(dstDir);
    const id = crypto.randomUUID();
    const label = `${up ? "Upload" : "Download"} ${paths.length === 1 ? basename(paths[0]) : `${paths.length} items`}`;
    setJobs((js) => [{ id, label, dstDir, file: "", files: 0, filesTotal: 0, bytes: 0, bytesTotal: 0, state: "running" }, ...js]);
    try {
      await api.sftpTransfer(id, paths.map((src) => ({ src, dstDir })));
    } catch (e) {
      setJobs((js) => js.map((j) => (j.id === id ? { ...j, state: "failed", error: errMsg(e) } : j)));
      toast.error(errMsg(e));
    }
  }, []);

  const active = useTabs((s) => s.activeId === tab.id);
  return (
    <div className="flex h-full flex-col bg-app">
      <div className="flex h-9 shrink-0 items-center gap-2 border-b border-line bg-panel px-3 text-[12px]">
        <Server className="size-3.5 text-ok" />
        <span className="font-semibold">{host}</span>
        <span className="text-faint">SFTP</span>
        <div className="flex-1" />
        <IconButton title={showHidden ? "Hide hidden files" : "Show hidden files"} active={showHidden} onClick={() => setShowHidden((v) => !v)}>
          {showHidden ? <Eye className="size-3.5" /> : <EyeOff className="size-3.5" />}
        </IconButton>
        <IconButton title="Open a terminal in the host's folder" disabled={!remoteDir} onClick={() => remoteDir && openTerminal(remoteDir)}>
          <SquareTerminal className="size-3.5" />
        </IconButton>
      </div>
      <Group orientation="vertical" className="min-h-0 flex-1">
        <Panel defaultSize="78%" minSize="30%">
          <Group orientation="horizontal" className="h-full">
            <Panel defaultSize="50%" minSize="20%">
              <FilePane
                side="local"
                title="This computer"
                dir={localDir}
                setDir={setLocalDir}
                reload={reload.local}
                showHidden={showHidden}
                active={active}
                otherDir={remoteDir}
                listings={listings.current}
                onTransfer={transfer}
              />
            </Panel>
            <Separator className="w-px bg-line hover:bg-accent data-[separator=active]:bg-accent" />
            <Panel defaultSize="50%" minSize="20%">
              <FilePane
                side="remote"
                title={host}
                dir={remoteDir}
                error={remoteError}
                setDir={setRemoteDir}
                reload={reload.remote}
                showHidden={showHidden}
                active={active}
                otherDir={localDir}
                listings={listings.current}
                onTransfer={transfer}
              />
            </Panel>
          </Group>
        </Panel>
        <Separator className="h-px bg-line hover:bg-accent data-[separator=active]:bg-accent" />
        <Panel minSize="8%">
          <Transfers jobs={jobs} onClear={() => setJobs((js) => js.filter((j) => j.state === "running"))} />
        </Panel>
      </Group>
    </div>
  );
}

function FilePane({ side, title, dir, error, setDir, reload, showHidden, active, otherDir, listings, onTransfer }: {
  side: Side;
  title: string;
  dir: string | null;
  error?: string | null;
  setDir: (d: string) => void;
  reload: number;
  showHidden: boolean;
  active: boolean;
  otherDir: string | null;
  listings: Record<Side, FileEntry[] | null>;
  onTransfer: (paths: string[], dstDir: string, existing: FileEntry[] | null) => void;
}) {
  const [entries, setEntries] = useState<FileEntry[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [anchor, setAnchor] = useState<string | null>(null);
  const [pathText, setPathText] = useState("");
  const [dropping, setDropping] = useState(false);
  const [nonce, setNonce] = useState(0);
  const menu = useContextMenu();
  const remote = side === "remote";
  const otherSide: Side = remote ? "local" : "remote";

  const shownPath = (d: string) => (remote ? remoteParts(d)?.path ?? d : d);
  useEffect(() => { if (dir) setPathText(shownPath(dir)); }, [dir]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    if (!dir) return;
    let cancelled = false;
    setLoading(true);
    api.fsList(dir, true)
      .then((list) => {
        if (cancelled) return;
        setEntries(list);
        setLoadError(null);
        listings[side] = list;
        setSelected((s) => new Set([...s].filter((p) => list.some((e) => e.path === p))));
      })
      .catch((e) => { if (!cancelled) setLoadError(errMsg(e)); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [dir, reload, nonce, side]);

  const visible = useMemo(() => (entries ?? []).filter((e) => showHidden || !e.name.startsWith(".")), [entries, showHidden]);
  const refresh = () => setNonce((n) => n + 1);

  const go = (text: string) => {
    const t = text.trim();
    if (!t || !dir) return;
    if (!remote) return setDir(t);
    const r = remoteParts(dir)!;
    // `~/x` is resolved by the host.
    const uri = remoteUri(r.dest, r.port, t);
    api.remoteResolveDir(uri).then(setDir).catch((e) => toast.error(errMsg(e)));
  };
  const up = () => { const p = dir && parentOf(dir); if (p) setDir(p); };
  const open = (e: FileEntry) => {
    if (e.isDir) setDir(e.path);
    else openTab({ type: "file", path: e.path }, e.name, `file:${e.path}`);
  };
  const selection = (e?: FileEntry) => (e && !selected.has(e.path) ? [e.path] : [...selected]);
  const sendOver = (paths: string[]) => { if (otherDir && paths.length) onTransfer(paths, otherDir, listings[otherSide]); };

  const newFolder = async () => {
    if (!dir) return;
    const name = await promptDialog({ title: "New folder", label: "Name", confirmLabel: "Create" });
    if (!name?.trim()) return;
    try { await api.sftpMkdir(joinPath(dir, name.trim())); refresh(); } catch (e) { toast.error(errMsg(e)); }
  };
  const rename = async (e: FileEntry) => {
    const name = await promptDialog({ title: `Rename ${e.name}`, label: "New name", defaultValue: e.name, confirmLabel: "Rename" });
    if (!name?.trim() || name.trim() === e.name) return;
    try { await api.sftpRename(e.path, name.trim()); refresh(); } catch (err) { toast.error(errMsg(err)); }
  };
  const remove = async (paths: string[]) => {
    if (!paths.length) return;
    const ok = await confirmDialog({
      title: paths.length === 1 ? `Delete ${basename(paths[0])}?` : `Delete ${paths.length} items?`,
      message: <>{remote ? `They're deleted on ${title} for good` : "They're deleted for good, not moved to the Trash"}; folders with everything in them.</>,
      confirmLabel: "Delete",
      danger: true,
    });
    if (!ok) return;
    try { await api.sftpDelete(paths); refresh(); } catch (err) { toast.error(errMsg(err)); }
  };

  const rowMenu = (ev: React.MouseEvent, e: FileEntry) => {
    if (!selected.has(e.path)) { setSelected(new Set([e.path])); setAnchor(e.path); }
    const paths = selection(e);
    const items: MenuItem[] = [
      { label: e.isDir ? "Open folder" : "Open", onClick: () => open(e) },
      {
        label: remote ? `Download to ${otherDir ? basename(otherDir) || otherDir : "…"}` : `Upload to ${otherDir ? basename(remoteParts(otherDir)?.path ?? "") || "/" : "…"}`,
        icon: remote ? <ArrowLeft className="size-3.5" /> : <ArrowRight className="size-3.5" />,
        disabled: !otherDir,
        onClick: () => sendOver(paths),
      },
      "separator",
      { label: "Rename…", icon: <Pencil className="size-3.5" />, shortcut: "F2", disabled: paths.length !== 1, onClick: () => void rename(e) },
      { label: "Copy path", icon: <Copy className="size-3.5" />, onClick: () => void copyText(remote ? remoteParts(e.path)?.path ?? e.path : e.path) },
      "separator",
      { label: paths.length > 1 ? `Delete ${paths.length} items…` : "Delete…", icon: <Trash2 className="size-3.5" />, danger: true, onClick: () => void remove(paths) },
    ];
    menu.show(ev, items);
  };
  const paneMenu = (ev: React.MouseEvent) => {
    menu.show(ev, [
      { label: "New folder…", icon: <FolderPlus className="size-3.5" />, onClick: () => void newFolder() },
      { label: "Refresh", icon: <RefreshCw className="size-3.5" />, shortcut: "F5", onClick: refresh },
    ]);
  };

  const click = (ev: React.MouseEvent, e: FileEntry) => {
    if (ev.shiftKey && anchor) {
      const names = visible.map((x) => x.path);
      const [a, b] = [names.indexOf(anchor), names.indexOf(e.path)].sort((x, y) => x - y);
      if (a >= 0) return setSelected(new Set(names.slice(a, b + 1)));
    }
    if (ev.metaKey || ev.ctrlKey) {
      setSelected((s) => { const n = new Set(s); if (n.has(e.path)) n.delete(e.path); else n.add(e.path); return n; });
    } else {
      setSelected(new Set([e.path]));
    }
    setAnchor(e.path);
  };
  const onKey = (ev: React.KeyboardEvent) => {
    if (!active || (ev.target as HTMLElement).tagName === "INPUT") return;
    const one = visible.find((e) => selected.has(e.path));
    if (ev.key === "Enter" && one) { ev.preventDefault(); open(one); }
    else if (ev.key === "Backspace" && !ev.metaKey) { ev.preventDefault(); up(); }
    else if (ev.key === "Delete" || (ev.key === "Backspace" && ev.metaKey)) { ev.preventDefault(); void remove([...selected]); }
    else if (ev.key === "F2" && one) { ev.preventDefault(); void rename(one); }
    else if (ev.key === "F5") { ev.preventDefault(); refresh(); }
    else if (ev.key === "a" && (ev.metaKey || ev.ctrlKey)) { ev.preventDefault(); setSelected(new Set(visible.map((e) => e.path))); }
  };

  // Drops: rows from the other pane (copy here), or files from Finder / Explorer (upload).
  const onDrop = async (ev: React.DragEvent) => {
    setDropping(false);
    if (!dir || !isFileDrag(ev.dataTransfer)) return;
    ev.preventDefault();
    const fromTree = ev.dataTransfer.getData(DRAG_PATHS);
    const paths = fromTree ? (JSON.parse(fromTree) as string[]) : remote ? await droppedPaths(ev.dataTransfer, "sftp") : [];
    // Only copies across: dropping a pane's own rows on itself does nothing.
    const across = paths.filter((p) => isRemotePath(p) !== remote);
    if (across.length) onTransfer(across, dir, entries);
  };

  const shownError = error ?? loadError;
  return (
    <div
      className="relative flex h-full min-w-0 flex-col outline-none"
      tabIndex={0}
      onKeyDown={onKey}
      onDragOver={(ev) => { if (isFileDrag(ev.dataTransfer)) { ev.preventDefault(); ev.dataTransfer.dropEffect = "copy"; setDropping(true); } }}
      onDragLeave={(ev) => { if (!ev.currentTarget.contains(ev.relatedTarget as Node)) setDropping(false); }}
      onDrop={(ev) => void onDrop(ev)}
    >
      <div className="flex h-9 shrink-0 items-center gap-1 border-b border-line px-2">
        {remote ? <Server className="size-3.5 shrink-0 text-ok" /> : <Monitor className="size-3.5 shrink-0 text-muted" />}
        <span className="max-w-32 shrink-0 truncate text-[11px] font-medium text-muted" title={title}>{title}</span>
        <IconButton title="Up (Backspace)" disabled={!dir || !parentOf(dir)} onClick={up}><ArrowUp className="size-3.5" /></IconButton>
        <form className="min-w-0 flex-1" onSubmit={(e) => { e.preventDefault(); go(pathText); }}>
          <Input className="h-6! font-mono text-[11px]!" value={pathText} onChange={(e) => setPathText(e.target.value)} disabled={!dir} />
        </form>
        <IconButton title="Refresh (F5)" disabled={!dir} onClick={refresh}>
          {loading ? <Loader2 className="size-3.5 animate-spin" /> : <RefreshCw className="size-3.5" />}
        </IconButton>
        <IconButton title="New folder" disabled={!dir} onClick={() => void newFolder()}><FolderPlus className="size-3.5" /></IconButton>
        <IconButton
          title={remote ? "Download the selection to this computer" : "Upload the selection to the host"}
          disabled={!selected.size || !otherDir}
          onClick={() => sendOver([...selected])}
        >
          {remote ? <ArrowLeft className="size-3.5 text-accent" /> : <ArrowRight className="size-3.5 text-accent" />}
        </IconButton>
      </div>
      <div className="grid shrink-0 grid-cols-[1fr_72px_88px] gap-2 border-b border-line px-3 py-1 text-[10px] font-medium uppercase tracking-wide text-faint">
        <span>Name</span><span className="text-right">Size</span><span className="text-right">Modified</span>
      </div>
      <div className="min-h-0 flex-1 overflow-auto" onContextMenu={(ev) => { if (ev.target === ev.currentTarget) paneMenu(ev); }} onMouseDown={(ev) => { if (ev.target === ev.currentTarget) setSelected(new Set()); }}>
        {shownError ? (
          <div className="selectable m-3 whitespace-pre-wrap rounded-md border border-danger/30 bg-danger/10 px-3 py-2 font-mono text-[11px] text-danger">{shownError}</div>
        ) : !entries ? (
          <div className="flex h-24 items-center justify-center"><Loader2 className="size-4 animate-spin text-muted" /></div>
        ) : (
          <>
            {visible.map((e) => (
              <div
                key={e.path}
                draggable
                onDragStart={(ev) => {
                  const paths = selected.has(e.path) ? [...selected] : [e.path];
                  ev.dataTransfer.setData(DRAG_PATHS, JSON.stringify(paths));
                  ev.dataTransfer.effectAllowed = "copy";
                }}
                onClick={(ev) => click(ev, e)}
                onDoubleClick={() => open(e)}
                onContextMenu={(ev) => rowMenu(ev, e)}
                className={clsx(
                  "grid cursor-default grid-cols-[1fr_72px_88px] items-center gap-2 px-3 py-[3px] text-[12px]",
                  selected.has(e.path) ? "bg-accent/15 text-fg" : "hover:bg-hover",
                )}
              >
                <span className="flex min-w-0 items-center gap-1.5">
                  <FileIcon name={e.name} isDir={e.isDir} />
                  <span className={clsx("truncate", e.name.startsWith(".") && "text-muted")}>{e.name}</span>
                </span>
                <span className="text-right text-[11px] text-faint tabular-nums">{e.isDir ? "" : fmtSize(e.size)}</span>
                <span className="truncate text-right text-[11px] text-faint" title={e.modified ?? undefined}>{relTime(e.modified)}</span>
              </div>
            ))}
            {!visible.length && <div className="px-3 py-6 text-center text-[12px] text-faint">Empty folder</div>}
            {/* Room to right-click the folder itself. */}
            <div className="h-10" onContextMenu={paneMenu} onMouseDown={() => setSelected(new Set())} />
          </>
        )}
      </div>
      {dropping && (
        <div className="pointer-events-none absolute inset-2 top-10 flex items-center justify-center rounded-lg border-2 border-dashed border-accent/60 bg-app/70 text-[13px] text-fg">
          {remote ? `Drop to upload to ${title}` : "Drop to download here"}
        </div>
      )}
      {menu.element}
    </div>
  );
}

function Transfers({ jobs, onClear }: { jobs: Job[]; onClear: () => void }) {
  return (
    <div className="flex h-full flex-col">
      <div className="flex h-7 shrink-0 items-center gap-2 border-b border-line px-3 text-[11px] font-medium text-muted">
        Transfers
        <div className="flex-1" />
        {jobs.some((j) => j.state !== "running") && <button type="button" className="text-faint hover:text-fg" onClick={onClear}>Clear finished</button>}
      </div>
      <div className="min-h-0 flex-1 overflow-auto">
        {!jobs.length && <div className="px-3 py-3 text-[11px] text-faint">Copies between this computer and the host show here. Drag files from one side to the other, or use the arrow buttons.</div>}
        {jobs.map((j) => {
          const pct = j.bytesTotal ? Math.min(100, (j.bytes / j.bytesTotal) * 100) : j.state === "done" ? 100 : 0;
          return (
            <div key={j.id} className="flex items-center gap-3 border-b border-line/60 px-3 py-1.5 text-[12px]">
              <span className="w-4 shrink-0">
                {j.state === "running" ? <Loader2 className="size-3.5 animate-spin text-accent" />
                  : j.state === "done" ? <Check className="size-3.5 text-ok" />
                  : <X className={clsx("size-3.5", j.state === "failed" ? "text-danger" : "text-faint")} />}
              </span>
              <span className="w-56 shrink-0 truncate" title={j.label}>{j.label}</span>
              <div className="h-1.5 min-w-16 flex-1 overflow-hidden rounded bg-raised">
                <div className={clsx("h-full rounded", j.state === "failed" ? "bg-danger" : "bg-accent")} style={{ width: `${pct}%` }} />
              </div>
              <span className="w-40 shrink-0 truncate text-right text-[11px] text-faint tabular-nums" title={j.error ?? j.file}>
                {j.state === "failed" ? j.error : j.state === "cancelled" ? "Cancelled" : `${j.files}/${j.filesTotal} · ${fmtSize(j.bytes)} of ${fmtSize(j.bytesTotal)}`}
              </span>
              {j.state === "running"
                ? <IconButton title="Cancel" onClick={() => void api.sftpCancel(j.id)}><X className="size-3.5" /></IconButton>
                : <span className="size-6 shrink-0" />}
            </div>
          );
        })}
      </div>
    </div>
  );
}
