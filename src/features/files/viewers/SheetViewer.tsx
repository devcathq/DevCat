// Spreadsheet / CSV preview: sheet tabs + the shared read-only DataGrid.
import clsx from "clsx";
import { AlertTriangle } from "lucide-react";
import { useMemo, useState } from "react";
import { createPortal } from "react-dom";
import { Checkbox, Select } from "../../../components/ui";
import type { CellValue, ColumnInfo, FilePreview, SheetData } from "../../../lib/types";
import DataGrid from "../../grid/DataGrid";
import { colLetter, detectDelimiter, MAX_CSV_ROWS, parseCsv } from "../csv";

const NUM = /^-?(?:\d{1,15}|\d{1,15}\.\d+|\.\d+)$/;

function toGrid(rows: string[][], header: boolean): { columns: ColumnInfo[]; rows: CellValue[][] } {
  const width = rows.reduce((m, r) => Math.max(m, r.length), 0);
  const head = header ? rows[0] ?? [] : [];
  const seen = new Map<string, number>();
  const columns: ColumnInfo[] = Array.from({ length: width }, (_, i) => {
    let name = (head[i] ?? "").trim() || colLetter(i);
    const n = seen.get(name) ?? 0;
    seen.set(name, n + 1);
    if (n) name = `${name} (${n + 1})`;
    return { name, typeName: colLetter(i) };
  });
  const body = header ? rows.slice(1) : rows;
  const data = body.map((r) => {
    const out: CellValue[] = new Array(width);
    for (let i = 0; i < width; i++) {
      const v = r[i] ?? "";
      out[i] = NUM.test(v) && !(v.length > 1 && v[0] === "0" && v[1] !== ".") ? Number(v) : v;
    }
    return out;
  });
  return { columns, rows: data };
}

function Grid({ sheet, header }: { sheet: SheetData; header: boolean }) {
  const grid = useMemo(() => toGrid(sheet.rows, header), [sheet, header]);
  if (!grid.columns.length) return <div className="p-4 text-xs text-faint italic">Empty sheet</div>;
  return (
    <DataGrid
      find
      columns={grid.columns}
      rows={grid.rows}
      footer={sheet.truncated && (
        <span className="inline-flex items-center gap-1 text-warn"><AlertTriangle className="size-3" /> Truncated preview</span>
      )}
    />
  );
}

export function SheetViewer({ preview, slot }: { preview: FilePreview; slot: HTMLElement | null }) {
  const sheets = preview.sheets ?? [];
  const [idx, setIdx] = useState(0);
  const [header, setHeader] = useState(true);
  const sheet = sheets[Math.min(idx, sheets.length - 1)];
  return (
    <div className="flex h-full min-h-0 flex-col">
      {slot && createPortal(<Checkbox label="Header row" checked={header} onChange={setHeader} />, slot)}
      {(preview.truncated || sheet?.truncated) && <TruncatedBanner text="Only the first rows of this workbook are shown." />}
      <div className="min-h-0 flex-1">{sheet ? <Grid key={idx} sheet={sheet} header={header} /> : <div className="p-4 text-xs text-faint italic">No sheets</div>}</div>
      {sheets.length > 1 && (
        <div className="flex h-7 shrink-0 items-stretch gap-px overflow-x-auto border-t border-line bg-panel px-1">
          {sheets.map((s, i) => (
            <button
              key={i}
              onClick={() => setIdx(i)}
              className={clsx(
                "shrink-0 border-t-2 px-3 text-[12px]",
                i === idx ? "border-accent bg-app text-fg" : "border-transparent text-muted hover:bg-hover hover:text-fg",
              )}
            >
              {s.name}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

const DELIMS: [string, string][] = [[",", "Comma"], [";", "Semicolon"], ["\t", "Tab"], ["|", "Pipe"]];

export function CsvViewer({ preview, slot }: { preview: FilePreview; slot: HTMLElement | null }) {
  const text = preview.text ?? "";
  const detected = useMemo(() => detectDelimiter(text, preview.path), [text, preview.path]);
  const [delim, setDelim] = useState<string | null>(null);
  const d = delim ?? detected;
  const [header, setHeader] = useState(true);
  const parsed = useMemo(() => parseCsv(text, d), [text, d]);
  const sheet = useMemo<SheetData>(() => ({ name: "csv", rows: parsed.rows, truncated: parsed.truncated }), [parsed]);
  return (
    <div className="flex h-full min-h-0 flex-col">
      {slot && createPortal(
        <>
          <Select className="h-6 w-28 text-xs" value={d} onChange={(e) => setDelim(e.target.value)} title="Delimiter">
            {DELIMS.map(([v, l]) => <option key={l} value={v}>{l}</option>)}
          </Select>
          <Checkbox label="Header row" checked={header} onChange={setHeader} />
        </>,
        slot,
      )}
      {(preview.truncated || parsed.truncated) && (
        <TruncatedBanner text={parsed.truncated ? `Showing the first ${MAX_CSV_ROWS.toLocaleString()} rows.` : "The file is large; only its beginning is shown."} />
      )}
      <div className="min-h-0 flex-1"><Grid sheet={sheet} header={header} /></div>
    </div>
  );
}

export function TruncatedBanner({ text }: { text: string }) {
  return (
    <div className="flex shrink-0 items-center gap-1.5 border-b border-warn/25 bg-warn/10 px-3 py-1 text-[12px] text-warn">
      <AlertTriangle className="size-3.5 shrink-0" /> {text}
    </div>
  );
}
