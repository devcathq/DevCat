// Virtualized, editable data grid shared by query results, table data, Mongo and compare views.
//
// Props:
//   columns / rows      result set. `rows` MUST be referentially stable (state/memo):
//                       a new array resets pending edits and selection.
//   primaryKey          key columns; editing is enabled only when `editable` and every key column is present.
//   controller          optional external state from useGridController() so a parent toolbar can
//                       add/delete rows, commit or discard. Created internally when omitted.
//   sort/onSortChange   controlled (server-side) sort. Without onSortChange the grid sorts client-side.
//   onCommit            called with RowChange[] on Cmd/Ctrl+S (parent applies them and reloads).
//   rowNumberOffset     first row number - 1 (paging).
//   dialect/tableName   used by "Copy as INSERT".
//   onFilterByValue     adds "Filter by this value" to the cell menu.
//   columnFilters       per-column "contains" text shown in a filter row under the header;
//   onColumnFilter      called with (column, text) on Enter ("" clears). The row shows only with both.
//   onActiveRowChange   data index of the focused row (e.g. for a form view).
//   find                Ctrl/⌘+F (while the grid is on screen) opens a find bar over every row.
//   footer              extra content at the right of the status bar.
import clsx from "clsx";
import {
  ArrowDown, ArrowUp, ClipboardCopy, Copy, CopyPlus, Eraser, Filter, KeyRound, Maximize2, Plus, Search, Trash2, Undo2, X,
} from "lucide-react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { toast } from "sonner";
import type { CellValue, ColumnInfo, Dialect, RowChange, SortSpec } from "../../lib/types";
import { useContextMenu, type MenuItem } from "../connections/ContextMenu";
import CellEditorDialog from "./CellEditorDialog";
import { copyText, serialize, type CopyFormat } from "./copy";
import { cellKind, displayText, editText, needsPopup, parseInput } from "./format";
import { FIND_KEY, FindBar, findMatches, firstFrom, focusFind } from "./GridFind";
import { useGridController, type GridController } from "./useGridController";

export interface DataGridProps {
  columns: ColumnInfo[];
  rows: CellValue[][];
  primaryKey?: string[];
  editable?: boolean;
  controller?: GridController;
  sort?: SortSpec[];
  onSortChange?: (sort: SortSpec[]) => void;
  onCommit?: (changes: RowChange[]) => void | Promise<unknown>;
  rowNumberOffset?: number;
  dialect?: Dialect;
  tableName?: string;
  onFilterByValue?: (column: string, value: CellValue) => void;
  columnFilters?: Record<string, string>;
  onColumnFilter?: (column: string, text: string) => void;
  onActiveRowChange?: (dataIndex: number | null) => void;
  find?: boolean;
  footer?: ReactNode;
  hideStatus?: boolean;
  className?: string;
}

const ROW_H = 22;
const HEAD_H = 34;
const FILTER_H = 24;
const CHAR_W = 7.2;

export default function DataGrid(props: DataGridProps) {
  const internal = useGridController({
    columns: props.columns, rows: props.rows, primaryKey: props.primaryKey, editable: props.editable,
    sort: props.sort, onSortChange: props.onSortChange,
  });
  const ctl = props.controller ?? internal;
  const { columns } = ctl;
  const filterRow = !!props.onColumnFilter && !!props.columnFilters;
  const headH = HEAD_H + (filterRow ? FILTER_H : 0);

  const scrollRef = useRef<HTMLDivElement>(null);
  const [widths, setWidths] = useState<number[]>(() => autoWidths(props.columns, props.rows));
  const colKey = columns.map((c) => c.name).join("\u0001");
  useEffect(() => { setWidths(autoWidths(columns, ctl.rows)); }, [colKey]); // eslint-disable-line react-hooks/exhaustive-deps

  const rowNumW = Math.max(40, String(ctl.rowCount + (props.rowNumberOffset ?? 0)).length * 8 + 18);

  const rowVirt = useVirtualizer({
    count: ctl.rowCount,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_H,
    overscan: 12,
    // rows start under the header (like columns after the row numbers), which stays on top of them
    paddingStart: headH,
    scrollPaddingStart: headH,
  });
  const colVirt = useVirtualizer({
    horizontal: true,
    count: columns.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: (i) => widths[i] ?? 120,
    overscan: 4,
    paddingStart: rowNumW,
    scrollPaddingStart: rowNumW,
  });
  useEffect(() => { colVirt.measure(); }, [widths, rowNumW]); // eslint-disable-line react-hooks/exhaustive-deps

  const [editing, setEditing] = useState<{ r: number; c: number; text: string; selectAll?: boolean } | null>(null);
  const [popup, setPopup] = useState<{ r: number; c: number } | null>(null);
  const menu = useContextMenu();
  const dragging = useRef(false);

  useEffect(() => {
    const up = () => { dragging.current = false; };
    window.addEventListener("mouseup", up);
    return () => window.removeEventListener("mouseup", up);
  }, []);

  // Report the active row.
  const focusRow = ctl.sel?.focus.r ?? null;
  const activeData = focusRow !== null ? ctl.order[focusRow] ?? null : null;
  const onActive = props.onActiveRowChange;
  useEffect(() => { onActive?.(activeData); }, [activeData, onActive]);

  const scrollTo = useCallback((r: number, c: number) => {
    rowVirt.scrollToIndex(r, { align: "auto" });
    colVirt.scrollToIndex(c, { align: "auto" });
  }, [rowVirt, colVirt]);

  const focusGrid = useCallback(() => scrollRef.current?.focus({ preventScroll: true }), []);

  // ---- find: matches over every row (in view order), the selected cell is the current one
  const rootRef = useRef<HTMLDivElement>(null);
  const findRef = useRef<HTMLInputElement>(null);
  const [findOpen, setFindOpen] = useState(false);
  const [findText, setFindText] = useState("");
  const [query, setQuery] = useState("");
  useEffect(() => {
    const t = setTimeout(() => setQuery(findText), 120);
    return () => clearTimeout(t);
  }, [findText]);
  const nCols = columns.length;
  const matches = useMemo(() => findMatches(ctl.order, nCols, ctl.value, findOpen ? query : ""), [ctl.order, nCols, ctl.value, findOpen, query]);
  const focusKey = ctl.sel ? ctl.sel.focus.r * nCols + ctl.sel.focus.c : -1;
  const focusKeyRef = useRef(focusKey);
  focusKeyRef.current = focusKey;
  const currentMatch = matches.set.has(focusKey) ? firstFrom(matches.keys, focusKey) : -1;

  const goToMatch = useCallback((k: number | undefined) => {
    if (k === undefined) return;
    const r = Math.floor(k / nCols), c = k % nCols;
    ctl.setSel({ anchor: { r, c }, focus: { r, c } });
    // a match off screen comes to the middle, as in a browser's find
    const el = scrollRef.current, top = headH + r * ROW_H;
    const shown = !!el && top >= el.scrollTop + headH && top + ROW_H <= el.scrollTop + el.clientHeight;
    rowVirt.scrollToIndex(r, { align: shown ? "auto" : "center" });
    colVirt.scrollToIndex(c, { align: "auto" });
  }, [ctl, nCols, headH, rowVirt, colVirt]);
  // New results: go to the first match from the selected cell on, as a browser's find does.
  useEffect(() => {
    const { keys } = matches;
    if (keys.length) goToMatch(keys[firstFrom(keys, Math.max(focusKeyRef.current, 0))] ?? keys[0]);
  }, [matches]); // eslint-disable-line react-hooks/exhaustive-deps
  const stepMatch = (dir: 1 | -1) => {
    const { keys } = matches;
    if (!keys.length) return;
    const i = dir > 0 ? firstFrom(keys, focusKey + 1) : firstFrom(keys, focusKey) - 1;
    goToMatch(keys[i] ?? keys[dir > 0 ? 0 : keys.length - 1]);
  };
  // (the bar focuses its input when it mounts)
  const openFind = useCallback(() => {
    setFindOpen(true);
    focusFind(findRef.current);
  }, []);
  const closeFind = () => {
    setFindOpen(false);
    focusGrid();
  };
  useEffect(() => {
    if (!props.find) return;
    const h = (e: KeyboardEvent) => {
      if (e.defaultPrevented || !(e.metaKey || e.ctrlKey) || e.altKey || e.shiftKey || e.key.toLowerCase() !== "f") return;
      // only the grid on screen (other tabs are display: none), and not from a field or an editor
      // elsewhere (those have their own find)
      if (!scrollRef.current?.getClientRects().length) return;
      const t = e.target;
      if (t instanceof Element && !rootRef.current?.contains(t) && t.closest("input, textarea, select, [contenteditable=true], .cm-editor, .xterm")) return;
      e.preventDefault();
      openFind();
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, [props.find, openFind]);

  // ---- editing
  const startEdit = useCallback((r: number, c: number, initial?: string) => {
    const d = ctl.order[r];
    if (d === undefined) return;
    const v = ctl.value(d, c);
    const col = columns[c];
    if (!ctl.canEdit || ctl.isDeleted(d)) {
      setPopup({ r, c });
      return;
    }
    if (initial === undefined && needsPopup(v ?? null, col.typeName)) {
      setPopup({ r, c });
      return;
    }
    setEditing({ r, c, text: initial ?? (v === undefined ? "" : editText(v)), selectAll: initial === undefined });
  }, [ctl, columns]);

  // Mirror of `editing` so blur + key handlers can't commit the same edit twice.
  const editRef = useRef(editing);
  editRef.current = editing;

  const commitEdit = useCallback((move?: "down" | "right" | "left", cancel = false, refocus = true) => {
    const ed = editRef.current;
    if (!ed) return;
    editRef.current = null;
    // Move focus back synchronously so keys typed right after Enter/Tab reach the grid.
    if (refocus) focusGrid();
    if (!cancel) {
      const d = ctl.order[ed.r];
      const orig = ctl.value(d, ed.c);
      const origText = orig === undefined || orig === null ? "" : editText(orig);
      if (ed.text !== origText || (orig === null && ed.text !== "")) {
        ctl.setCell(d, ed.c, parseInput(ed.text, orig ?? null, columns[ed.c].typeName));
      }
    }
    setEditing(null);
    if (move) {
      const r = move === "down" ? Math.min(ed.r + 1, ctl.rowCount - 1) : ed.r;
      const c = move === "right" ? Math.min(ed.c + 1, columns.length - 1) : move === "left" ? Math.max(ed.c - 1, 0) : ed.c;
      ctl.setSel({ anchor: { r, c }, focus: { r, c } });
      scrollTo(r, c);
    }
  }, [ctl, columns, scrollTo]);

  // ---- clipboard
  const doCopy = useCallback(async (format: CopyFormat, all = false) => {
    const block = ctl.selectedBlock(all);
    if (!block.rows.length) return;
    await copyText(serialize(format, block.columns, block.rows, { dialect: props.dialect, tableName: props.tableName }));
    toast.success(`Copied ${block.rows.length} row${block.rows.length === 1 ? "" : "s"}`, { duration: 1200 });
  }, [ctl, props.dialect, props.tableName]);

  const setNullOnSelection = useCallback(() => {
    if (!ctl.canEdit || !ctl.range) return;
    for (let r = ctl.range.r0; r <= ctl.range.r1; r++) {
      const d = ctl.order[r];
      if (ctl.isDeleted(d)) continue;
      for (let c = ctl.range.c0; c <= ctl.range.c1; c++) ctl.setCell(d, c, null);
    }
  }, [ctl]);

  const toggleSort = useCallback((name: string) => {
    const cur = ctl.sort.find((s) => s.column === name);
    if (!cur) ctl.setSort([{ column: name, desc: false }]);
    else if (!cur.desc) ctl.setSort([{ column: name, desc: true }]);
    else ctl.setSort([]);
  }, [ctl]);

  // ---- keyboard
  const onKeyDown = (e: React.KeyboardEvent) => {
    if (editing || e.target !== scrollRef.current) return;
    const mod = e.metaKey || e.ctrlKey;
    const sel = ctl.sel;
    const nRows = ctl.rowCount, nCols = columns.length;
    if (mod && e.key.toLowerCase() === "a") { e.preventDefault(); ctl.selectAll(); return; }
    if (mod && e.key.toLowerCase() === "c") { e.preventDefault(); void doCopy(e.shiftKey ? "tsv-header" : "tsv"); return; }
    if (mod && e.key.toLowerCase() === "s" && props.onCommit && ctl.pendingCount) {
      e.preventDefault(); void props.onCommit(ctl.buildChanges()); return;
    }
    if (mod && (e.key === "Backspace" || e.key === "Delete") && ctl.canEdit) {
      e.preventDefault(); ctl.deleteRows(ctl.selectedDataRows()); return;
    }
    if (!sel) {
      if (["ArrowDown", "ArrowUp", "ArrowLeft", "ArrowRight", "Home", "End"].includes(e.key) && nRows && nCols) {
        e.preventDefault();
        ctl.setSel({ anchor: { r: 0, c: 0 }, focus: { r: 0, c: 0 } });
      }
      return;
    }
    const f = sel.focus;
    const page = Math.max(1, Math.floor((scrollRef.current?.clientHeight ?? 400) / ROW_H) - 2);
    let next: { r: number; c: number } | null = null;
    switch (e.key) {
      case "ArrowDown": next = { r: mod ? nRows - 1 : f.r + 1, c: f.c }; break;
      case "ArrowUp": next = { r: mod ? 0 : f.r - 1, c: f.c }; break;
      case "ArrowRight": next = { r: f.r, c: mod ? nCols - 1 : f.c + 1 }; break;
      case "ArrowLeft": next = { r: f.r, c: mod ? 0 : f.c - 1 }; break;
      case "PageDown": next = { r: f.r + page, c: f.c }; break;
      case "PageUp": next = { r: f.r - page, c: f.c }; break;
      case "Home": next = { r: mod ? 0 : f.r, c: 0 }; break;
      case "End": next = { r: mod ? nRows - 1 : f.r, c: nCols - 1 }; break;
      case "Tab": next = { r: f.r, c: e.shiftKey ? f.c - 1 : f.c + 1 }; break;
      case "Enter": case "F2": e.preventDefault(); startEdit(f.r, f.c); return;
      case "Escape": ctl.setSel(null); return;
      case "Delete": case "Backspace":
        if (ctl.canEdit) { e.preventDefault(); startEdit(f.r, f.c, ""); }
        return;
      default:
        if (e.key.length === 1 && !mod && !e.altKey && ctl.canEdit && !ctl.isDeleted(ctl.order[f.r])) {
          e.preventDefault();
          startEdit(f.r, f.c, e.key);
        }
        return;
    }
    e.preventDefault();
    next = { r: clamp(next.r, 0, nRows - 1), c: clamp(next.c, 0, nCols - 1) };
    const extend = e.shiftKey && e.key !== "Tab";
    ctl.setSel({ anchor: extend ? sel.anchor : next, focus: next });
    scrollTo(next.r, next.c);
  };

  // ---- mouse
  const onCellMouseDown = (e: React.MouseEvent, r: number, c: number) => {
    if (e.button !== 0) return;
    if (editing) commitEdit(undefined, false, false);
    dragging.current = true;
    const sel = ctl.sel;
    ctl.setSel(e.shiftKey && sel ? { anchor: sel.anchor, focus: { r, c } } : { anchor: { r, c }, focus: { r, c } });
  };
  const onCellEnter = (r: number, c: number) => {
    if (dragging.current && ctl.sel) ctl.setSel({ anchor: ctl.sel.anchor, focus: { r, c } });
  };
  const onRowHeaderDown = (e: React.MouseEvent, r: number) => {
    if (e.button !== 0) return;
    dragging.current = true;
    const sel = ctl.sel;
    const last = columns.length - 1;
    ctl.setSel(e.shiftKey && sel
      ? { anchor: { r: sel.anchor.r, c: 0 }, focus: { r, c: last } }
      : { anchor: { r, c: 0 }, focus: { r, c: last } });
  };

  const inRange = (r: number, c: number) => {
    const g = ctl.range;
    return !!g && r >= g.r0 && r <= g.r1 && c >= g.c0 && c <= g.c1;
  };

  const openMenu = (e: React.MouseEvent, r: number, c: number) => {
    e.preventDefault();
    if (!inRange(r, c)) ctl.setSel({ anchor: { r, c }, focus: { r, c } });
    const d = ctl.order[r];
    const v = ctl.value(d, c) ?? null;
    const col = columns[c];
    const items: MenuItem[] = [
      { label: "Copy", icon: <Copy className="size-3.5" />, shortcut: "⌘C", onClick: () => void doCopy("tsv") },
      {
        label: "Copy as", icon: <ClipboardCopy className="size-3.5" />, submenu: [
          { label: "TSV with headers", onClick: () => void doCopy("tsv-header") },
          { label: "CSV", onClick: () => void doCopy("csv") },
          { label: "JSON", onClick: () => void doCopy("json") },
          { label: "Markdown table", onClick: () => void doCopy("markdown") },
          { label: "SQL INSERT statements", onClick: () => void doCopy("insert") },
          "separator",
          { label: "Column name", onClick: () => void copyText(col.name) },
        ],
      },
      { label: ctl.canEdit ? "Open in editor" : "View value", icon: <Maximize2 className="size-3.5" />, onClick: () => setPopup({ r, c }) },
    ];
    if (props.onFilterByValue) {
      items.push({ label: "Filter by this value", icon: <Filter className="size-3.5" />, onClick: () => props.onFilterByValue!(col.name, v) });
    }
    items.push("separator",
      { label: "Sort ascending", icon: <ArrowUp className="size-3.5" />, onClick: () => ctl.setSort([{ column: col.name, desc: false }]) },
      { label: "Sort descending", icon: <ArrowDown className="size-3.5" />, onClick: () => ctl.setSort([{ column: col.name, desc: true }]) },
    );
    if (ctl.sort.length) items.push({ label: "Clear sort", onClick: () => ctl.setSort([]) });
    if (ctl.canEdit) {
      const rowsSel = ctl.selectedDataRows();
      const allDeleted = rowsSel.length > 0 && rowsSel.every((x) => ctl.isDeleted(x));
      items.push("separator",
        { label: "Set NULL", icon: <Eraser className="size-3.5" />, onClick: setNullOnSelection },
        { label: "Add row", icon: <Plus className="size-3.5" />, onClick: () => addRowAndFocus() },
        { label: rowsSel.length > 1 ? `Duplicate ${rowsSel.length} rows` : "Duplicate row", icon: <CopyPlus className="size-3.5" />, onClick: () => ctl.duplicateRows(rowsSel) },
        allDeleted
          ? { label: "Restore row", icon: <Undo2 className="size-3.5" />, onClick: () => ctl.deleteRows(rowsSel) }
          : { label: rowsSel.length > 1 ? `Delete ${rowsSel.length} rows` : "Delete row", icon: <Trash2 className="size-3.5" />, danger: true, shortcut: "⌘⌫", onClick: () => ctl.deleteRows(rowsSel) },
      );
    }
    menu.show(e, items);
  };

  const addRowAndFocus = () => ctl.addRow();

  // Focus the last row whenever new rows are added (toolbar, menu or duplicate).
  const insertedCount = ctl.rowCount - ctl.rows.length;
  const prevInserted = useRef(insertedCount);
  useEffect(() => {
    if (insertedCount > prevInserted.current) {
      const r = ctl.rowCount - 1;
      ctl.setSel({ anchor: { r, c: 0 }, focus: { r, c: 0 } });
      rowVirt.scrollToIndex(r, { align: "auto" });
      colVirt.scrollToIndex(0, { align: "auto" });
      focusGrid();
    }
    prevInserted.current = insertedCount;
  }, [insertedCount]); // eslint-disable-line react-hooks/exhaustive-deps

  // ---- column resize
  const startResize = (e: React.MouseEvent, c: number) => {
    e.preventDefault();
    e.stopPropagation();
    const x0 = e.clientX, w0 = widths[c];
    const move = (ev: MouseEvent) => setWidths((ws) => ws.map((w, i) => (i === c ? Math.max(40, w0 + ev.clientX - x0) : w)));
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      document.body.style.cursor = "";
    };
    document.body.style.cursor = "col-resize";
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };
  const autosize = (c: number) => {
    const sample = ctl.order.slice(0, 500).map((d) => ctl.value(d, c) ?? null);
    const w = measureCol(columns[c], sample, 800);
    setWidths((ws) => ws.map((x, i) => (i === c ? w : x)));
  };

  const vRows = rowVirt.getVirtualItems();
  const vCols = colVirt.getVirtualItems();
  const totalW = colVirt.getTotalSize();
  const totalH = rowVirt.getTotalSize();
  const pkSet = useMemo(() => new Set(ctl.pkIdx), [ctl.pkIdx]);
  const editingPopup = popup && ctl.order[popup.r] !== undefined ? popup : null;

  return (
    <div ref={rootRef} className={clsx("flex h-full min-h-0 flex-col bg-panel", props.className)}>
      {findOpen && (
        <FindBar
          inputRef={findRef}
          text={findText}
          onText={setFindText}
          matches={matches}
          current={currentMatch}
          searching={findText !== query}
          onStep={stepMatch}
          onClose={closeFind}
        />
      )}
      <div
        ref={scrollRef}
        tabIndex={0}
        onKeyDown={onKeyDown}
        className="no-select relative min-h-0 flex-1 overflow-auto font-mono text-xs outline-none"
      >
        <div style={{ width: totalW, height: totalH, position: "relative" }}>
          {/* header */}
          <div className="sticky top-0 z-20 flex border-b border-line bg-raised" style={{ height: headH, width: totalW }}>
            <div
              className="sticky left-0 z-30 flex shrink-0 cursor-default flex-col border-r border-line bg-raised text-[10px] text-faint"
              style={{ width: rowNumW }}
            >
              <div className="flex flex-1 items-end justify-end px-1.5 pb-1" style={{ height: HEAD_H }} onMouseDown={() => { ctl.selectAll(); }} title="Select all">#</div>
              {filterRow && (
                <div className="flex shrink-0 items-center justify-center border-t border-line" style={{ height: FILTER_H }} title="Filter: type in a column and press Enter">
                  <Search className="size-3" />
                </div>
              )}
            </div>
            {vCols.map((vc) => {
              const col = columns[vc.index];
              const s = ctl.sort.find((x) => x.column === col.name);
              return (
                <div
                  key={vc.key}
                  className="group absolute top-0 flex cursor-pointer flex-col justify-center border-r border-line px-2 font-sans hover:bg-hover"
                  style={{ left: vc.start, width: vc.size, height: HEAD_H }}
                  onClick={() => toggleSort(col.name)}
                  title={`${col.name} : ${col.typeName}\nClick to sort`}
                >
                  <div className="flex min-w-0 items-center gap-1">
                    {pkSet.has(vc.index) && <KeyRound className="size-3 shrink-0 text-accent" />}
                    <span className="truncate text-xs font-semibold text-fg">{col.name}</span>
                    {s && (s.desc ? <ArrowDown className="size-3 shrink-0 text-accent" /> : <ArrowUp className="size-3 shrink-0 text-accent" />)}
                  </div>
                  <div className="truncate font-mono text-[10px] leading-tight text-faint">{col.typeName || " "}</div>
                  <div
                    className="absolute top-0 right-[-3px] z-10 h-full w-1.5 cursor-col-resize hover:bg-accent/60"
                    onMouseDown={(e) => startResize(e, vc.index)}
                    onClick={(e) => e.stopPropagation()}
                    onDoubleClick={(e) => { e.stopPropagation(); autosize(vc.index); }}
                  />
                </div>
              );
            })}
            {filterRow && vCols.map((vc) => {
              const name = columns[vc.index].name;
              return (
                <ColumnFilterInput
                  key={`f:${vc.key}`}
                  column={name}
                  value={props.columnFilters![name] ?? ""}
                  onApply={(t) => props.onColumnFilter!(name, t)}
                  style={{ left: vc.start, width: vc.size, top: HEAD_H, height: FILTER_H }}
                />
              );
            })}
          </div>

          {/* rows */}
          {vRows.map((vr) => {
            const r = vr.index;
            const d = ctl.order[r];
            const inserted = ctl.isInserted(d);
            const deleted = ctl.isDeleted(d);
            const rowSelected = !!ctl.range && r >= ctl.range.r0 && r <= ctl.range.r1;
            return (
              <div
                key={vr.key}
                className={clsx(
                  "absolute left-0 flex",
                  inserted ? "bg-ok/10" : deleted ? "bg-danger/10" : r % 2 ? "bg-white/[0.015]" : "",
                )}
                style={{ top: vr.start, height: ROW_H, width: totalW }}
              >
                <div
                  className={clsx(
                    "sticky left-0 z-10 flex shrink-0 cursor-default items-center justify-end border-r border-b border-line/60 bg-panel px-1.5 text-[10px] tabular-nums",
                    rowSelected ? "text-accent" : "text-faint",
                    inserted && "text-ok",
                    deleted && "text-danger",
                  )}
                  style={{ width: rowNumW }}
                  onMouseDown={(e) => onRowHeaderDown(e, r)}
                  onMouseEnter={() => dragging.current && ctl.sel && ctl.setSel({ anchor: ctl.sel.anchor, focus: { r, c: columns.length - 1 } })}
                  onContextMenu={(e) => openMenu(e, r, 0)}
                >
                  {inserted ? "+" : (props.rowNumberOffset ?? 0) + d + 1}
                </div>
                {vCols.map((vc) => {
                  const c = vc.index;
                  const isEditing = editing && editing.r === r && editing.c === c;
                  const v = ctl.value(d, c);
                  const selected = inRange(r, c);
                  const focused = ctl.sel?.focus.r === r && ctl.sel?.focus.c === c;
                  const modified = ctl.isModified(d, c) || (inserted && v !== undefined);
                  const found = matches.set.has(r * nCols + c);
                  return (
                    <div
                      key={vc.key}
                      className={clsx(
                        "absolute top-0 flex h-full items-center overflow-hidden border-r border-b border-line/60 px-2",
                        modified && !inserted && !found && "bg-warn/15",
                        found ? (focused ? "bg-warn/45" : "bg-warn/20") : selected && "bg-accent/15",
                        focused && "outline outline-1 -outline-offset-1 outline-accent",
                        deleted && "text-danger/80 line-through",
                      )}
                      style={{ left: vc.start, width: vc.size }}
                      onMouseDown={(e) => onCellMouseDown(e, r, c)}
                      onMouseEnter={() => onCellEnter(r, c)}
                      onDoubleClick={() => startEdit(r, c)}
                      onContextMenu={(e) => openMenu(e, r, c)}
                    >
                      {isEditing ? (
                        <input
                          autoFocus
                          className="absolute inset-0 h-full w-full border border-accent bg-app px-2 font-mono text-xs text-fg outline-none"
                          value={editing.text}
                          onFocus={(e) => editing.selectAll && e.currentTarget.select()}
                          onChange={(e) => setEditing({ ...editing, text: e.target.value, selectAll: false })}
                          onBlur={() => commitEdit(undefined, false, false)}
                          onKeyDown={(e) => {
                            e.stopPropagation();
                            if (e.key === "Enter") { e.preventDefault(); commitEdit("down"); }
                            else if (e.key === "Tab") { e.preventDefault(); commitEdit(e.shiftKey ? "left" : "right"); }
                            else if (e.key === "Escape") { e.preventDefault(); commitEdit(undefined, true); }
                          }}
                        />
                      ) : (
                        <CellView value={v} typeName={columns[c].typeName} onExpand={() => setPopup({ r, c })} />
                      )}
                    </div>
                  );
                })}
              </div>
            );
          })}
        </div>
        {ctl.rowCount === 0 && (
          <div className="pointer-events-none absolute inset-x-0 flex justify-center font-sans text-xs text-faint" style={{ top: headH + 24 }}>
            {columns.length ? "No rows" : "No columns"}
          </div>
        )}
      </div>

      {!props.hideStatus && (
        <div className="flex h-6 shrink-0 items-center gap-3 border-t border-line bg-app px-2 text-[11px] text-muted">
          <span className="tabular-nums">{ctl.rows.length.toLocaleString()} row{ctl.rows.length === 1 ? "" : "s"}</span>
          {ctl.range && <SelectionInfo ctl={ctl} />}
          {ctl.pendingCount > 0 && (
            <span className="text-warn">{ctl.pendingCount} pending change{ctl.pendingCount === 1 ? "" : "s"}</span>
          )}
          <span className="flex-1" />
          {props.footer}
          {props.find && (
            <button type="button" className="-mr-1 inline-flex items-center gap-1 rounded px-1 hover:bg-hover hover:text-fg" title={`Find in all rows (${FIND_KEY})`} onClick={openFind}>
              <Search className="size-3" />Find
            </button>
          )}
        </div>
      )}

      {editingPopup && (
        <CellEditorDialog
          column={columns[editingPopup.c].name}
          typeName={columns[editingPopup.c].typeName}
          value={ctl.value(ctl.order[editingPopup.r], editingPopup.c)}
          readOnly={!ctl.canEdit || ctl.isDeleted(ctl.order[editingPopup.r])}
          onSave={(v) => ctl.setCell(ctl.order[editingPopup.r], editingPopup.c, v)}
          onClose={() => { setPopup(null); requestAnimationFrame(focusGrid); }}
        />
      )}
      {menu.element}
    </div>
  );
}

/** One column's "contains" filter: edits a draft, applies on Enter, Escape reverts. */
function ColumnFilterInput({ column, value, onApply, style }: {
  column: string; value: string; onApply: (text: string) => void; style: React.CSSProperties;
}) {
  const [draft, setDraft] = useState(value);
  useEffect(() => { setDraft(value); }, [value]);
  const pending = draft !== value;
  return (
    <div className="absolute flex items-center border-t border-r border-line bg-panel px-0.5" style={style}>
      <input
        spellCheck={false}
        className={clsx(
          "h-[18px] min-w-0 flex-1 rounded-sm border bg-app px-1 font-mono text-[11px] text-fg placeholder:text-faint focus:outline-none",
          value ? "border-accent/70" : pending ? "border-warn/60" : "border-transparent focus:border-line",
        )}
        placeholder="contains…"
        title={`Rows where ${column} contains this text (Enter to filter, Esc to revert)`}
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === "Enter") { e.preventDefault(); onApply(draft); }
          else if (e.key === "Escape") { e.preventDefault(); setDraft(value); }
        }}
      />
      {value && (
        <button
          className="ml-0.5 shrink-0 rounded-sm p-px text-muted hover:bg-hover hover:text-fg"
          title="Clear this filter"
          onClick={() => onApply("")}
        >
          <X className="size-3" />
        </button>
      )}
    </div>
  );
}

function CellView({ value, typeName, onExpand }: { value: CellValue | undefined; typeName: string; onExpand: () => void }) {
  if (value === undefined) return <span className="text-faint italic">DEFAULT</span>;
  const kind = cellKind(value, typeName);
  const text = displayText(value, kind);
  switch (kind) {
    case "null": return <span className="text-faint italic">NULL</span>;
    case "number": return <span className="w-full truncate text-right tabular-nums text-info">{text}</span>;
    case "bool": return <span className={value ? "text-ok" : "text-danger/80"}>{text}</span>;
    case "binary": return <span className="truncate text-muted italic">{text}</span>;
    case "json":
      return (
        <>
          <span className="min-w-0 flex-1 truncate text-warn/90">{text}</span>
          <button
            className="ml-1 shrink-0 rounded px-0.5 text-[10px] text-muted hover:bg-hover hover:text-fg"
            onMouseDown={(e) => e.stopPropagation()}
            onClick={(e) => { e.stopPropagation(); onExpand(); }}
            title="Expand"
          >
            {Array.isArray(value) ? "[…]" : "{…}"}
          </button>
        </>
      );
    default: return <span className="truncate">{text}</span>;
  }
}

function SelectionInfo({ ctl }: { ctl: GridController }) {
  const g = ctl.range!;
  const nr = g.r1 - g.r0 + 1, nc = g.c1 - g.c0 + 1;
  if (nr === 1 && nc === 1) return <span>R{g.r0 + 1} · {ctl.columns[g.c0]?.name}</span>;
  // sum of numeric cells for small selections
  let sum = 0, count = 0;
  if (nr * nc <= 50000) {
    for (let r = g.r0; r <= g.r1; r++) for (let c = g.c0; c <= g.c1; c++) {
      const v = ctl.value(ctl.order[r], c);
      if (typeof v === "number") { sum += v; count++; }
    }
  }
  return (
    <span className="tabular-nums">
      {nr.toLocaleString()} × {nc} selected
      {count > 1 && <> · Σ {+sum.toFixed(6)} · avg {+(sum / count).toFixed(4)}</>}
    </span>
  );
}

function clamp(n: number, lo: number, hi: number) {
  return Math.max(lo, Math.min(hi, n));
}

function measureCol(col: ColumnInfo, sample: (CellValue)[], max = 360): number {
  let chars = Math.max(col.name.length + 3, Math.min(col.typeName.length, 18));
  for (const v of sample) {
    const t = displayText(v, cellKind(v, col.typeName));
    const len = cellKind(v, col.typeName) === "json" ? Math.min(t.length, 40) + 3 : t.length;
    if (len > chars) chars = len;
    if (chars * CHAR_W > max) break;
  }
  return Math.round(clamp(chars * CHAR_W + 20, 56, max));
}

function autoWidths(columns: ColumnInfo[], rows: CellValue[][]): number[] {
  const sample = rows.slice(0, 100);
  return columns.map((col, i) => measureCol(col, sample.map((r) => r[i] ?? null)));
}
