// Sidebar tree of saved connections and their (lazily loaded) database objects.
// State lives in tree/store.ts; rows are flattened in tree/rows.ts and virtualized here.
import { useVirtualizer } from "@tanstack/react-virtual";
import clsx from "clsx";
import { Cable, ChevronsDownUp, FolderMinus, FolderPlus, Plus, RefreshCw, Search, Star, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState, type DragEvent, type KeyboardEvent, type MouseEvent } from "react";
import { Button, Empty, IconButton } from "../../components/ui";
import { api } from "../../lib/api";
import { IS_MAIN_WINDOW } from "../../lib/windows";
import { useConnections } from "../../store/connections";
import { useUi } from "../../store/ui";
import { openBookmark } from "./bookmarks/actions";
import { useBookmarks } from "./bookmarks/store";
import { useAutoExpandBookmarks } from "./bookmarks/useAutoExpand";
import { useContextMenu } from "./ContextMenu";
import { activateConnection, activateNode, refreshAll, toggleConnection, toggleNode } from "./tree/actions";
import { backgroundMenu, rowMenu } from "./tree/menus";
import { openMcpSync, suggestMcpSync } from "./mcp/McpSyncDialog";
import { createProject, DRAG_MIME, moveConnections, newConnectionIn } from "./tree/projectActions";
import { buildRows, isExpandable, isExpanded, isInfra, type Row, type SelectableRow } from "./tree/rows";
import { nodeKey, useTree } from "./tree/store";
import { ROW_H, TreeRow } from "./tree/TreeRow";

const selectable = (r: Row): r is SelectableRow => r.type !== "error" && r.type !== "empty";

/** Project a dragged connection lands in when dropped on `row` (null = no project, undefined = not a drop target). */
function dropProjectOf(row: Row | undefined): string | null | undefined {
  if (!row) return null;
  if (row.type === "bookmarks" || row.type === "bookmark") return undefined;
  if (row.type === "project") return row.project.id;
  if (row.type === "empty" && row.parentId.startsWith("p:")) return row.parentId.slice(2);
  return "conn" in row ? row.conn.projectId ?? null : null;
}

export default function ConnectionTree() {
  const allConnections = useConnections((s) => s.connections);
  const open = useConnections((s) => s.open);
  const markOpen = useConnections((s) => s.markOpen);
  const nodes = useTree((s) => s.nodes);
  const connecting = useTree((s) => s.connecting);
  const allProjects = useConnections((s) => s.projects);
  const focusId = useConnections((s) => s.focusProjectId);
  // Once there are connections: offer Sync MCP (main window only, until it's set up or declined)
  const hasConnections = allConnections.length > 0;
  useEffect(() => {
    if (hasConnections && IS_MAIN_WINDOW) void suggestMcpSync();
  }, [hasConnections]);
  // Focus mode: only the focused project and its connections are visible.
  const projects = useMemo(() => (focusId ? allProjects.filter((p) => p.id === focusId) : allProjects), [allProjects, focusId]);
  const connections = useMemo(
    () => (focusId ? allConnections.filter((c) => c.projectId === focusId) : allConnections),
    [allConnections, focusId],
  );
  const collapsedProjects = useTree((s) => s.collapsedProjects);
  const selected = useTree((s) => s.selected);
  const bookmarks = useBookmarks((s) => s.bookmarks);
  const bookmarksOnly = useTree((s) => s.bookmarksOnly);
  const bookmarksCollapsed = useTree((s) => s.bookmarksCollapsed);
  const revealId = useTree((s) => s.revealId);
  const setSelected = useTree((s) => s.setSelected);
  const openDialog = useUi((s) => s.openConnectionDialog);
  const [query, setQuery] = useState("");
  const [focused, setFocused] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const menu = useContextMenu();
  const showMenu = menu.show;

  // Sync backend sessions that survived a frontend reload.
  useEffect(() => {
    api.openConnectionIds().then((ids) => ids.forEach((id) => markOpen(id, true))).catch(() => {});
  }, [markOpen]);

  // A session closed elsewhere (e.g. connection edited & saved): drop its cached subtree.
  useEffect(() => {
    const t = useTree.getState();
    for (const c of connections) {
      const root = t.nodes[nodeKey(c.id, [])];
      if (root?.expanded && !root.error && !open.has(c.id) && !t.connecting[c.id]) t.clearConn(c.id);
    }
  }, [open, connections]);

  // Bookmarks: load, and reload when connections change (deleting one deletes its bookmarks).
  useEffect(() => {
    void useBookmarks.getState().load();
  }, [allConnections]);

  useAutoExpandBookmarks(bookmarks, bookmarksOnly, nodes);

  const rows = useMemo(
    () => buildRows({ connections, projects, nodes, collapsedProjects, query, bookmarks, bookmarksCollapsed, bookmarksOnly }),
    [connections, projects, nodes, collapsedProjects, query, bookmarks, bookmarksCollapsed, bookmarksOnly],
  );

  // Lazily (re)load expanded nodes that have no cached children (e.g. after a refresh).
  useEffect(() => {
    const t = useTree.getState();
    for (const r of rows) {
      if (r.type === "conn" && r.expanded && open.has(r.conn.id)) {
        const st = t.nodes[nodeKey(r.conn.id, [])];
        if (st && !st.children && !st.loading && !st.error) void t.load(r.conn.id, []);
      } else if (r.type === "node" && r.expanded) {
        const st = t.nodes[nodeKey(r.conn.id, r.node.path)];
        if (!st?.children && !st?.loading && !st?.error) void t.load(r.conn.id, r.node.path);
      }
    }
  }, [rows, open]);

  const virt = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_H,
    overscan: 16,
    getItemKey: (i) => rows[i].id,
  });

  const indexOf = useCallback((id: string | null) => (id ? rows.findIndex((r) => r.id === id) : -1), [rows]);

  // A bookmark was revealed: scroll to its row once it's rendered.
  useEffect(() => {
    if (!revealId) return;
    const i = indexOf(revealId);
    if (i < 0) return;
    virt.scrollToIndex(i, { align: "center" });
    useTree.setState({ revealId: null });
    scrollRef.current?.focus();
  }, [revealId, indexOf, virt]);

  const toggle = useCallback((row: Row) => {
    if (row.type === "project") useTree.getState().toggleProject(row.project.id);
    else if (row.type === "bookmarks") useTree.getState().toggleBookmarks();
    else if (row.type === "conn") toggleConnection(row.conn);
    else if (row.type === "node") toggleNode(row.conn, row.node, row.expanded);
  }, []);

  const activate = useCallback((row: Row) => {
    if (row.type === "project") useTree.getState().toggleProject(row.project.id);
    else if (row.type === "bookmarks") useTree.getState().toggleBookmarks();
    else if (row.type === "bookmark") openBookmark(row.bookmark, row.conn);
    else if (row.type === "conn") activateConnection(row.conn);
    else if (row.type === "node") activateNode(row.conn, row.node, row.ctx, row.expanded);
  }, []);

  const refreshRow = useCallback((row: Row) => {
    const t = useTree.getState();
    if (row.type === "conn" && !isInfra(row.conn)) t.refresh(row.conn.id, []);
    else if (row.type === "node") t.refresh(row.conn.id, row.node.hasChildren ? row.node.path : row.parentPath);
    else void refreshAll();
  }, []);

  const onSelect = useCallback((row: Row) => setSelected(row.id), [setSelected]);

  const onRowContextMenu = useCallback((e: MouseEvent, row: Row) => {
    setSelected(row.id);
    const items = rowMenu(row);
    if (items) showMenu(e, items);
  }, [showMenu, setSelected]);

  const onRetry = useCallback((row: Extract<Row, { type: "error" }>) => {
    const t = useTree.getState();
    if (row.path.length === 0) t.refresh(row.conn.id, []);
    else void t.load(row.conn.id, row.path);
  }, []);

  // ---- drag a connection onto a project (or onto "No project") to move it
  const [dragging, setDragging] = useState<string | null>(null);
  /** project id under the pointer; null = "no project"; undefined = not over a target */
  const [dropTarget, setDropTarget] = useState<string | null | undefined>(undefined);
  const rowAt = (e: DragEvent) => {
    const id = (e.target as HTMLElement).closest<HTMLElement>("[data-row]")?.dataset.row;
    return id ? rows.find((r) => r.id === id) : undefined;
  };
  const endDrag = () => { setDragging(null); setDropTarget(undefined); };
  const dragHandlers = {
    onDragStart: (e: DragEvent) => {
      const row = rowAt(e);
      if (row?.type !== "conn") return e.preventDefault();
      e.dataTransfer.setData(DRAG_MIME, row.conn.id);
      e.dataTransfer.setData("text/plain", row.conn.name);
      e.dataTransfer.effectAllowed = "move";
      setDragging(row.conn.id);
    },
    onDragOver: (e: DragEvent) => {
      if (!dragging) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = "move";
      const zone = (e.target as HTMLElement).closest("[data-drop-none]");
      const target = zone ? null : dropProjectOf(rowAt(e));
      if (target !== dropTarget) setDropTarget(target);
    },
    onDrop: (e: DragEvent) => {
      e.preventDefault();
      const id = e.dataTransfer.getData(DRAG_MIME) || dragging;
      const conn = id ? connections.find((c) => c.id === id) : undefined;
      const zone = (e.target as HTMLElement).closest("[data-drop-none]");
      const target = zone ? null : dropProjectOf(rowAt(e));
      endDrag();
      if (conn && target !== undefined) void moveConnections([conn], target);
    },
    onDragEnd: endDrag,
  };
  const draggedConn = dragging ? connections.find((c) => c.id === dragging) : undefined;

  const moveTo = (i: number) => {
    const r = rows[i];
    if (!r) return;
    setSelected(r.id);
    virt.scrollToIndex(i, { align: "auto" });
  };

  const onKeyDown = (e: KeyboardEvent) => {
    const idx = indexOf(selected);
    const row = rows[idx];
    const step = (dir: 1 | -1) => {
      for (let i = idx + dir; i >= 0 && i < rows.length; i += dir) if (selectable(rows[i])) return moveTo(i);
    };
    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        if (idx < 0) { const first = rows.findIndex(selectable); if (first >= 0) moveTo(first); } else step(1);
        break;
      case "ArrowUp":
        e.preventDefault();
        if (idx < 0) return;
        if (idx === 0 || !rows.slice(0, idx).some(selectable)) searchRef.current?.focus();
        else step(-1);
        break;
      case "ArrowRight": {
        e.preventDefault();
        if (!row) return;
        const expanded = isExpanded(row);
        if (isExpandable(row) && !expanded) toggle(row);
        else if (expanded && rows[idx + 1]?.parentId === row.id && selectable(rows[idx + 1])) moveTo(idx + 1);
        break;
      }
      case "ArrowLeft": {
        e.preventDefault();
        if (!row) return;
        if (isExpanded(row)) toggle(row);
        else if (row.parentId) moveTo(indexOf(row.parentId));
        break;
      }
      case "Enter":
        e.preventDefault();
        if (row) activate(row);
        break;
      case "Home": e.preventDefault(); moveTo(rows.findIndex(selectable)); break;
      case "End": e.preventDefault(); { let i = rows.length - 1; while (i >= 0 && !selectable(rows[i])) i--; moveTo(i); } break;
      case "F5":
        e.preventDefault();
        if (row) refreshRow(row); else void refreshAll();
        break;
      case "ContextMenu":
        if (row) {
          e.preventDefault();
          const el = scrollRef.current?.querySelector<HTMLElement>(`[data-row="${CSS.escape(row.id)}"]`);
          const rect = el?.getBoundingClientRect();
          const items = rowMenu(row);
          if (items && rect) menu.show({ clientX: rect.left + 24, clientY: rect.bottom }, items);
        }
        break;
      default:
        if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "r") {
          e.preventDefault();
          if (row) refreshRow(row); else void refreshAll();
        }
    }
  };

  if (connections.length === 0 && projects.length === 0) {
    return (
      <Empty title="No connections yet">
        <p className="mb-3">Add a database, Kubernetes cluster, Docker engine or RabbitMQ broker, or create a project to organize them.</p>
        <div className="flex justify-center gap-2">
          <Button variant="primary" onClick={() => openDialog(null)}><Plus className="size-3.5" />New connection</Button>
          <Button onClick={() => void createProject()}><FolderPlus className="size-3.5" />New project</Button>
        </div>
      </Empty>
    );
  }

  return (
    <div className="flex h-full flex-col">
      <div className="flex shrink-0 items-center gap-1 border-b border-line px-2 py-1.5">
        <div className="relative min-w-0 flex-1">
          <Search className="pointer-events-none absolute top-1/2 left-2 size-3.5 -translate-y-1/2 text-faint" />
          <input
            ref={searchRef}
            value={query}
            spellCheck={false}
            placeholder="Filter connections & objects"
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") setQuery("");
              else if (e.key === "ArrowDown" || e.key === "Enter") {
                e.preventDefault();
                const first = rows.findIndex(selectable);
                if (first >= 0) moveTo(first);
                scrollRef.current?.focus();
              }
            }}
            className="h-6 w-full rounded-md border border-line bg-app pr-6 pl-7 text-[12px] text-fg placeholder:text-faint focus:border-accent focus:outline-none"
          />
          {query && (
            <button className="absolute top-1/2 right-1.5 -translate-y-1/2 text-faint hover:text-fg" onClick={() => setQuery("")} aria-label="Clear filter">
              <X className="size-3" />
            </button>
          )}
        </div>
        <IconButton title="New project" onClick={() => void createProject()}><FolderPlus className="size-3.5" /></IconButton>
        <IconButton title="Sync MCP: let Claude Code, Codex, Cursor and Gemini use these connections" onClick={openMcpSync}><Cable className="size-3.5" /></IconButton>
        <IconButton title="Show only bookmarked" active={bookmarksOnly} onClick={() => useTree.getState().setBookmarksOnly(!bookmarksOnly)}>
          <Star className={clsx("size-3.5", bookmarksOnly && "fill-current")} />
        </IconButton>
        <IconButton title="Refresh all" onClick={() => void refreshAll()}><RefreshCw className="size-3.5" /></IconButton>
        <IconButton title="Collapse all" onClick={() => useTree.getState().collapseAll()}><ChevronsDownUp className="size-3.5" /></IconButton>
      </div>

      <div
        ref={scrollRef}
        tabIndex={0}
        className="min-h-0 flex-1 overflow-auto py-1 outline-none focus-visible:outline-none"
        onKeyDown={onKeyDown}
        onFocus={() => setFocused(true)}
        onBlur={() => setFocused(false)}
        onContextMenu={(e) => {
          if (!(e.target as HTMLElement).closest("[data-row]")) menu.show(e, backgroundMenu());
        }}
        {...dragHandlers}
      >
        {rows.length === 0 ? (
          <div className="px-3 py-4 text-center text-xs text-muted">No matches for “{query}”</div>
        ) : (
          <div className="relative w-full" style={{ height: virt.getTotalSize() }}>
            {virt.getVirtualItems().map((vi) => {
              const row = rows[vi.index];
              return (
                <div key={vi.key} className="absolute inset-x-0 top-0" style={{ height: ROW_H, transform: `translateY(${vi.start}px)` }}>
                  <TreeRow
                    row={row}
                    selected={row.id === selected}
                    focused={focused}
                    isOpen={row.type === "conn" && open.has(row.conn.id)}
                    connecting={row.type === "conn" && !!connecting[row.conn.id]}
                    dropTarget={!!dragging && row.type === "project" && dropTarget === row.project.id && draggedConn?.projectId !== row.project.id}
                    onNewConnection={newConnectionIn}
                    onSelect={onSelect}
                    onToggle={toggle}
                    onActivate={activate}
                    onContextMenu={onRowContextMenu}
                    onRetry={onRetry}
                  />
                </div>
              );
            })}
          </div>
        )}
        {draggedConn?.projectId && (
          <div
            data-drop-none
            className={clsx(
              "mx-2 mt-2 flex h-8 items-center justify-center gap-1.5 rounded-md border border-dashed text-[12px]",
              dropTarget === null ? "border-accent bg-accent/10 text-fg" : "border-line text-muted",
            )}
          >
            <FolderMinus className="size-3.5" />Drop here to remove from project
          </div>
        )}
      </div>
      {menu.element}
    </div>
  );
}
