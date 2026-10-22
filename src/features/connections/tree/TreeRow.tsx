import clsx from "clsx";
import { AlertTriangle, ChevronRight, Folder, FolderOpen, Loader2, Lock, Plus, Star } from "lucide-react";
import { memo, type MouseEvent } from "react";
import { KindIcon } from "../../../components/ui/KindIcon";
import { NetBadge } from "../../network/NetBadge";
import { isNoteNode, NodeIcon } from "./icons";
import { bookmarkHint, isInfra, type Row } from "./rows";

export const ROW_H = 24;
const INDENT = 14;

interface Props {
  row: Row;
  selected: boolean;
  focused: boolean;
  isOpen: boolean;
  connecting: boolean;
  /** a dragged connection would be dropped into this project row */
  dropTarget?: boolean;
  onSelect: (row: Row) => void;
  onToggle: (row: Row) => void;
  onActivate: (row: Row) => void;
  onContextMenu: (e: MouseEvent, row: Row) => void;
  onRetry: (row: Extract<Row, { type: "error" }>) => void;
  onNewConnection?: (projectId: string) => void;
}

function Chevron({ expanded, visible, loading, onClick }: { expanded: boolean; visible: boolean; loading?: boolean; onClick: () => void }) {
  if (loading) return <span className="flex size-4 shrink-0 items-center justify-center"><Loader2 className="size-3 animate-spin text-muted" /></span>;
  if (!visible) return <span className="size-4 shrink-0" />;
  return (
    <span
      className="flex size-4 shrink-0 items-center justify-center rounded text-faint hover:text-fg"
      onClick={(e) => { e.stopPropagation(); onClick(); }}
      onDoubleClick={(e) => e.stopPropagation()}
    >
      <ChevronRight className={clsx("size-3 transition-transform duration-100", expanded && "rotate-90")} />
    </span>
  );
}

export const TreeRow = memo(function TreeRow({ row, selected, focused, isOpen, connecting, dropTarget, onSelect, onToggle, onActivate, onContextMenu, onRetry, onNewConnection }: Props) {
  const pad = 6 + row.depth * INDENT;

  if (row.type === "error") {
    return (
      <div className="flex h-6 items-center gap-1.5 pr-2 text-[12px]" style={{ paddingLeft: pad + 16 }} title={row.message}>
        <AlertTriangle className="size-3.5 shrink-0 text-danger" />
        <span className="min-w-0 flex-1 truncate text-danger/90">{row.message}</span>
        <button className="shrink-0 text-[11px] text-accent hover:underline" onClick={() => onRetry(row)}>Retry</button>
      </div>
    );
  }
  if (row.type === "empty") {
    return <div className="flex h-6 items-center truncate pr-2 text-[12px] italic text-faint" style={{ paddingLeft: pad + 16 }}>{row.text ?? "No items"}</div>;
  }

  const common = {
    "data-row": row.id,
    onMouseDown: (e: MouseEvent) => { if (e.button === 0) onSelect(row); },
    onDoubleClick: () => onActivate(row),
    onContextMenu: (e: MouseEvent) => onContextMenu(e, row),
    className: clsx(
      "no-select group relative flex h-6 cursor-default items-center gap-1.5 pr-2 text-[13px]",
      selected ? (focused ? "bg-accent/20 text-fg" : "bg-hover") : "hover:bg-hover/60",
    ),
    style: { paddingLeft: pad },
  };

  if (row.type === "project") {
    const p = row.project;
    const Icon = row.collapsed ? Folder : FolderOpen;
    return (
      <div
        {...common}
        className={clsx(common.className, "font-medium", dropTarget && "bg-accent/15 ring-1 ring-accent/60 ring-inset")}
        title={p.description ? `${p.name} — ${p.description}` : p.name}
      >
        <Chevron expanded={!row.collapsed} visible onClick={() => onToggle(row)} />
        <Icon className="size-3.5 shrink-0" style={{ color: p.color ?? "var(--color-muted)" }} />
        <span className="min-w-0 flex-1 truncate">{p.name}</span>
        <button
          title="New connection in this project"
          aria-label="New connection in this project"
          className="hidden size-4 shrink-0 items-center justify-center rounded text-muted group-hover:flex hover:bg-hover hover:text-fg"
          onMouseDown={(e) => e.stopPropagation()}
          onDoubleClick={(e) => e.stopPropagation()}
          onClick={(e) => { e.stopPropagation(); onNewConnection?.(p.id); }}
        >
          <Plus className="size-3" />
        </button>
        <span className="min-w-4 text-right text-[11px] font-normal text-faint">{row.count}</span>
      </div>
    );
  }

  if (row.type === "bookmarks") {
    return (
      <div {...common} className={clsx(common.className, "font-medium")}>
        <Chevron expanded={!row.collapsed} visible onClick={() => onToggle(row)} />
        <Star className="size-3.5 shrink-0 fill-warn text-warn" />
        <span className="min-w-0 flex-1 truncate">Bookmarks</span>
        <span className="min-w-4 text-right text-[11px] font-normal text-faint">{row.count}</span>
      </div>
    );
  }

  if (row.type === "bookmark") {
    const b = row.bookmark;
    const hint = bookmarkHint(b, row.conn);
    return (
      <div {...common} title={`${b.label} — ${hint}`}>
        <span className="size-4 shrink-0" />
        <NodeIcon node={{ path: b.path, label: b.label, kind: b.kind, hasChildren: false, table: b.table }} expanded={false} />
        <span className="min-w-0 shrink truncate">{b.label}</span>
        <span className="min-w-0 flex-1 truncate text-[11px] text-faint">{hint}</span>
      </div>
    );
  }

  if (row.type === "conn") {
    const c = row.conn;
    const infra = isInfra(c);
    const where = c.kind === "ssh" && c.user ? `${c.user}@${c.host}` : c.host;
    return (
      <div {...common} draggable title={c.host ? `${c.name} — ${where}${c.port ? `:${c.port}` : ""}` : c.name}>
        {c.color && <span className="absolute inset-y-0.5 left-0 w-[3px] rounded-r" style={{ background: c.color }} />}
        <Chevron expanded={row.expanded} visible={!infra} loading={connecting} onClick={() => onToggle(row)} />
        <span className="relative shrink-0">
          <KindIcon kind={c.kind} className="size-3.5" />
          {isOpen && <span className="absolute -right-0.5 -bottom-0.5 size-1.5 rounded-full bg-ok ring-1 ring-panel" />}
        </span>
        <span className={clsx("min-w-0 flex-1 truncate", isOpen || infra ? "text-fg" : "text-fg/80")}>{c.name}</span>
        {c.options?.networkId && <NetBadge id={c.options.networkId} />}
        {c.readOnly && <Lock className="size-3 shrink-0 text-faint" aria-label="Read-only" />}
      </div>
    );
  }

  const n = row.node;
  return (
    <div {...common} title={n.detail ? `${n.label} — ${n.detail}` : n.label}>
      <Chevron expanded={row.expanded} visible={n.hasChildren} loading={row.loading} onClick={() => onToggle(row)} />
      <NodeIcon node={n} expanded={row.expanded} />
      <span className={clsx("min-w-0 flex-1 truncate", isNoteNode(n) ? "text-muted italic" : n.kind === "folder" && "text-fg/85")}>{n.label}</span>
      {row.bookmarked && <Star className="size-3 shrink-0 fill-warn text-warn" aria-label="Bookmarked" />}
      {n.detail && <span className="max-w-[45%] shrink-0 truncate pl-2 font-mono text-[11px] text-faint">{n.detail}</span>}
    </div>
  );
});
