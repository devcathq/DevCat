// The room's files as a tree: folders are the `/` in names. Right-click a file or folder for what
// can be done with it (the room tab gives the menu); drag a file or folder onto a folder to move
// it there, or drop files from the computer onto a folder to share them in it.
import clsx from "clsx";
import { Bot, Brain, ChevronDown, ChevronRight, FileText, Folder, FolderOpen, Image as ImageIcon, PenTool, Sparkles } from "lucide-react";
import { useMemo, useState, type DragEvent, type ReactNode } from "react";
import { useContextMenu, type MenuItem } from "../connections/ContextMenu";
import { isFileDrag } from "../files/drop";
import type { Claim, FileInfo, MemberView } from "./api";

/** Dragging a room file or folder (its path) inside the tree. */
const ROOM_PATH = "application/x-devcat-room-path";

interface Node { name: string; path: string; file?: FileInfo; children: Map<string, Node> }

function build(files: FileInfo[]): Node {
  const root: Node = { name: "", path: "", children: new Map() };
  for (const f of files) {
    const parts = f.path.split("/");
    let at = root;
    parts.forEach((part, i) => {
      const path = parts.slice(0, i + 1).join("/");
      let next = at.children.get(part);
      if (!next) {
        next = { name: part, path, children: new Map() };
        at.children.set(part, next);
      }
      if (i === parts.length - 1) next.file = f;
      at = next;
    });
  }
  return root;
}

/** Folders first, then files, by name. */
function sorted(n: Node): Node[] {
  return [...n.children.values()].sort((a, b) => Number(!!a.file && !a.children.size) - Number(!!b.file && !b.children.size) || a.name.localeCompare(b.name));
}

const count = (n: Node): number => (n.file ? 1 : 0) + [...n.children.values()].reduce((s, c) => s + count(c), 0);

function folderIcon(path: string, open: boolean): ReactNode {
  if (path === "memory") return <Brain className="size-3.5 shrink-0 text-accent" />;
  if (path === "skills") return <Sparkles className="size-3.5 shrink-0 text-accent" />;
  return open ? <FolderOpen className="size-3.5 shrink-0" /> : <Folder className="size-3.5 shrink-0" />;
}

function fileIcon(f: FileInfo): ReactNode {
  if (f.kind === "board") return <PenTool className="size-3.5 shrink-0" />;
  if (f.kind === "blob") return <ImageIcon className="size-3.5 shrink-0" />;
  return <FileText className="size-3.5 shrink-0" />;
}

export interface TreeTarget { path: string; folder: boolean; file?: FileInfo }

export function FileTree({ files, current, onOpen, members, claims, canEdit, menu, onMove, onDropFiles }: {
  files: FileInfo[];
  current: string | null;
  onOpen: (path: string) => void;
  members: MemberView[];
  claims: Claim[];
  canEdit: boolean;
  /** What right-clicking a file or folder offers. */
  menu: (t: TreeTarget) => MenuItem[];
  /** A file or folder dragged onto a folder ("" for the top). */
  onMove: (from: string, toFolder: string) => void;
  /** Files from the computer dropped onto a folder. */
  onDropFiles: (dt: DataTransfer, folder: string) => void;
}) {
  const tree = useMemo(() => build(files), [files]);
  const [closed, setClosed] = useState<Set<string>>(() => new Set());
  const [over, setOver] = useState<string | null>(null);
  const ctx = useContextMenu();
  const toggle = (p: string) => setClosed((s) => {
    const n = new Set(s);
    if (n.has(p)) n.delete(p); else n.add(p);
    return n;
  });

  const dropProps = (folder: string) => canEdit ? {
    onDragOver: (e: DragEvent) => {
      if (e.dataTransfer.types.includes(ROOM_PATH) || isFileDrag(e.dataTransfer)) {
        e.preventDefault();
        e.stopPropagation();
        setOver(folder);
      }
    },
    onDragLeave: (e: DragEvent) => { if (!e.currentTarget.contains(e.relatedTarget as globalThis.Node)) setOver((o) => (o === folder ? null : o)); },
    onDrop: (e: DragEvent) => {
      const moved = e.dataTransfer.getData(ROOM_PATH);
      if (!moved && !isFileDrag(e.dataTransfer)) return;
      e.preventDefault();
      e.stopPropagation();
      setOver(null);
      if (moved) onMove(moved, folder);
      else onDropFiles(e.dataTransfer, folder);
    },
  } : {};

  const row = (n: Node, depth: number): ReactNode => {
    const isFolder = n.children.size > 0 && !n.file;
    const pad = { paddingLeft: 6 + depth * 12 };
    const drag = canEdit ? { draggable: true, onDragStart: (e: DragEvent) => { e.dataTransfer.setData(ROOM_PATH, n.path); e.dataTransfer.effectAllowed = "move"; } } : {};
    if (isFolder) {
      const open = !closed.has(n.path);
      return (
        <div key={`d:${n.path}`} {...dropProps(n.path)}>
          <button {...drag} style={pad} onClick={() => toggle(n.path)} onContextMenu={(e) => ctx.show(e, menu({ path: n.path, folder: true }))}
            className={clsx("flex w-full items-center gap-1 rounded py-1 pr-2 text-left text-[12.5px] text-muted hover:bg-hover hover:text-fg", over === n.path && "bg-accent/20 text-fg ring-1 ring-accent/50")}>
            {open ? <ChevronDown className="size-3 shrink-0" /> : <ChevronRight className="size-3 shrink-0" />}
            {folderIcon(n.path, open)}
            <span className="truncate">{n.name}</span>
            <span className="ml-auto text-[10px] text-faint">{count(n)}</span>
          </button>
          {open && sorted(n).map((c) => row(c, depth + 1))}
        </div>
      );
    }
    const f = n.file!;
    const viewers = members.filter((m) => !m.me && m.online && m.file === f.path);
    const claimed = claims.some((c) => c.path === f.path && c.winning);
    return (
      <div key={`f:${n.path}`} {...drag} style={pad} onContextMenu={(e) => ctx.show(e, menu({ path: f.path, folder: false, file: f }))}
        className={clsx("group flex items-center gap-1.5 rounded py-1 pr-2 text-[12.5px]", f.path === current ? "bg-accent/15 text-fg" : "text-muted hover:bg-hover hover:text-fg")}>
        <span className="w-3 shrink-0" />
        <button className="flex min-w-0 flex-1 items-center gap-1.5 text-left" onClick={() => onOpen(f.path)} title={f.path}>
          {fileIcon(f)}
          <span className="truncate">{n.name}</span>
        </button>
        {claimed && <Bot className="size-3 shrink-0 text-accent" aria-label="An agent is editing part of it" />}
        {viewers.length > 0 && <span className="text-[10px] text-ok" title={viewers.map((v) => v.name).join(", ")}>{viewers.length}</span>}
      </div>
    );
  };

  return (
    <div className={clsx("min-h-full pb-6", over === "" && "rounded bg-accent/10 ring-1 ring-accent/40")} {...dropProps("")}>
      {sorted(tree).map((n) => row(n, 0))}
      {ctx.element}
    </div>
  );
}
