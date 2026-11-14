import Editor from "@monaco-editor/react";
import { Play } from "lucide-react";
import { useEffect, useState } from "react";
import { Button, Checkbox, Input, Select } from "../../components/ui";
import { api, errMsg } from "../../lib/api";
import type { ConnectionKind, TableRef } from "../../lib/types";
import { useConnections } from "../../store/connections";
import type { Job } from "./JobLog";
import { DELIMITERS, PathField, Row, Section, Segmented } from "./form";
import { SQL_KINDS, TableChecklist, TargetPicker, joinPath, listTables, lsGet, lsSet, safeFileName, useAsync, type Target } from "./shared";

type Format = "csv" | "tsv" | "json" | "jsonl" | "sql" | "xlsx";
const EXT: Record<Format, string> = { csv: "csv", tsv: "tsv", json: "json", jsonl: "jsonl", sql: "sql", xlsx: "xlsx" };
const FORMAT_LABEL: Record<Format, string> = { csv: "CSV", tsv: "TSV (tab separated)", json: "JSON (array)", jsonl: "JSON Lines", sql: "SQL INSERT statements", xlsx: "Excel (.xlsx)" };
const DIALECTS = [["", "Same as source"], ["postgres", "PostgreSQL"], ["mysql", "MySQL / MariaDB"], ["sqlite", "SQLite"], ["mssql", "SQL Server"]] as const;
// JDBC: export only (its tables can be read, but DevCat doesn't write SQL for it)
const KINDS: ConnectionKind[] = [...SQL_KINDS, "mongodb", "jdbc"];

interface FmtOpts {
  delimiter: string; header: boolean; quote: string; nullText: string; lineEnding: "\n" | "\r\n"; bom: boolean; jsonPretty: boolean;
  sqlIncludeCreate: boolean; sqlIncludeDrop: boolean; sqlTableName: string; targetDialect: string; sheetName: string;
}
const DEFAULT_OPTS: FmtOpts = {
  delimiter: ",", header: true, quote: '"', nullText: "", lineEnding: "\n", bom: false, jsonPretty: true,
  sqlIncludeCreate: true, sqlIncludeDrop: false, sqlTableName: "", targetDialect: "", sheetName: "",
};

export function ExportPanel({ initial, job }: { initial: Target; job: Job }) {
  const [src, setSrc] = useState<Target>(initial);
  const conn = useConnections((s) => s.connections.find((c) => c.id === src.connId));
  const isMongo = conn?.kind === "mongodb";
  const [source, setSource] = useState<"tables" | "query">("tables");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [query, setQuery] = useState("");
  const [format, setFormat] = useState<Format>(() => lsGet<Format>("transfer:exportFormat", "csv"));
  const [opts, setOpts] = useState<FmtOpts>(() => ({ ...DEFAULT_OPTS, ...lsGet<Partial<FmtOpts>>("transfer:exportOpts", {}) }));
  const [path, setPath] = useState("");
  const [where, setWhere] = useState("");
  const tables = useAsync(src.connId ? () => listTables(src.connId, src.database, src.schema) : null, [src.connId, src.database, src.schema]);
  useEffect(() => setSelected(new Set()), [src.connId, src.database, src.schema]);

  const chosen: TableRef[] = (tables.data ?? []).filter((t) => selected.has(t.name));
  const multi = source === "tables" && chosen.length > 1;
  const single = source === "query" ? "query" : chosen[0]?.name;
  // Reset path kind when switching between file and folder output
  useEffect(() => { setPath(""); }, [multi]);
  useEffect(() => { if (isMongo && format === "sql") setFormat("json"); }, [isMongo, format]);

  const set = <K extends keyof FmtOpts>(k: K, v: FmtOpts[K]) => {
    const next = { ...opts, [k]: v };
    setOpts(next);
    lsSet("transfer:exportOpts", next);
  };
  const setFmt = (f: Format) => { setFormat(f); lsSet("transfer:exportFormat", f); };

  const options = (): Record<string, unknown> => {
    const o: Record<string, unknown> = {};
    if (format === "csv" || format === "tsv") {
      Object.assign(o, { header: opts.header, quote: opts.quote, nullText: opts.nullText, lineEnding: opts.lineEnding, bom: opts.bom });
      o.delimiter = format === "tsv" ? "\t" : opts.delimiter;
    }
    if (format === "json") o.jsonPretty = opts.jsonPretty;
    if (format === "xlsx") Object.assign(o, { header: opts.header, ...(opts.sheetName ? { sheetName: opts.sheetName } : {}) });
    if (format === "sql") {
      Object.assign(o, { sqlIncludeCreate: opts.sqlIncludeCreate, includeDrop: opts.sqlIncludeDrop });
      if (opts.targetDialect) o.targetDialect = opts.targetDialect;
      if (opts.sqlTableName && !multi) o.sqlTableName = opts.sqlTableName;
    }
    if (source === "tables" && !multi && where.trim()) o.filter = where.trim();
    if (source === "query" && src.schema) o.schema = src.schema;
    return o;
  };

  const canRun = !!src.connId && !!path && (source === "query" ? query.trim().length > 0 : chosen.length > 0) && !job.job.running;

  const run = async () => {
    const jobId = job.start(source === "query" ? `Export query → ${path}` : `Export ${chosen.length} table(s) as ${format.toUpperCase()}`);
    let total = 0;
    try {
      if (source === "query") {
        total = await api.exportData({ connId: src.connId, query, database: src.database, format, path, options: { ...options(), jobId } });
        job.append(`Wrote ${total.toLocaleString()} rows to ${path}`, "ok");
      } else {
        let failed = 0;
        for (const t of chosen) {
          job.setStep(t.name);
          const file = multi ? joinPath(path, `${safeFileName(t.name)}.${EXT[format]}`) : path;
          try {
            const n = await api.exportData({
              connId: src.connId,
              table: { ...t, database: t.database ?? src.database, schema: t.schema ?? src.schema },
              database: src.database, format, path: file, options: { ...options(), jobId },
            });
            total += n;
            job.append(`${t.name}: ${n.toLocaleString()} rows → ${file}`, "ok");
          } catch (e) {
            failed++;
            job.append(`${t.name}: ${errMsg(e)}`, "error");
          }
        }
        if (failed) return job.finish(false, `Export finished with ${failed} error(s); ${total.toLocaleString()} rows written.`);
      }
      job.finish(true, `Export complete: ${total.toLocaleString()} rows.`);
    } catch (e) {
      job.finish(false, `Export failed: ${errMsg(e)}`);
    }
  };

  const formats: Format[] = isMongo ? ["json", "jsonl", "csv", "tsv", "xlsx"] : ["csv", "tsv", "json", "jsonl", "sql", "xlsx"];

  return (
    <div className="grid gap-3 lg:grid-cols-2">
      <Section title="Source">
        <TargetPicker value={src} onChange={setSrc} kinds={KINDS} />
        <div className="mt-3 flex items-center gap-2">
          <Segmented value={source} onChange={setSource} options={[{ value: "tables", label: isMongo ? "Collections" : "Tables" }, { value: "query", label: "Custom query" }]} />
        </div>
        <div className="mt-2 h-64">
          {source === "tables" ? (
            <TableChecklist className="h-full" tables={tables.data ?? []} loading={tables.loading} error={tables.error} selected={selected} onChange={setSelected}
              emptyText={src.connId ? "No tables found" : "Choose a connection"} />
          ) : (
            <div className="h-full overflow-hidden rounded-md border border-line">
              <Editor language={isMongo ? "javascript" : "sql"} theme="devcat" value={query} onChange={(v) => setQuery(v ?? "")}
                options={{ minimap: { enabled: false }, fontSize: 12, lineNumbers: "off", scrollBeyondLastLine: false, automaticLayout: true, wordWrap: "on" }} />
            </div>
          )}
        </div>
      </Section>

      <div className="flex flex-col gap-3">
        <Section title="Format">
          <Row label="Format">
            <Select value={format} onChange={(e) => setFmt(e.target.value as Format)}>
              {formats.map((f) => <option key={f} value={f}>{FORMAT_LABEL[f]}</option>)}
            </Select>
          </Row>
          {(format === "csv" || format === "tsv") && (
            <>
              {format === "csv" && <Row label="Delimiter">
                <Select value={opts.delimiter} onChange={(e) => set("delimiter", e.target.value)}>
                  {DELIMITERS.map((d) => <option key={d.label} value={d.value}>{d.label}</option>)}
                </Select>
              </Row>}
              <Row label="Quote character"><Input value={opts.quote} maxLength={1} onChange={(e) => set("quote", e.target.value)} className="w-16 font-mono" /></Row>
              <Row label="NULL as"><Input value={opts.nullText} onChange={(e) => set("nullText", e.target.value)} placeholder="(empty)" className="w-32 font-mono" /></Row>
              <Row label="Line ending">
                <Segmented value={opts.lineEnding} onChange={(v) => set("lineEnding", v)} options={[{ value: "\n", label: "LF" }, { value: "\r\n", label: "CRLF" }]} />
              </Row>
              <Row label="">
                <div className="flex flex-wrap gap-x-4 gap-y-1">
                  <Checkbox label="Include header row" checked={opts.header} onChange={(v) => set("header", v)} />
                  <Checkbox label="UTF-8 BOM (Excel)" checked={opts.bom} onChange={(v) => set("bom", v)} />
                </div>
              </Row>
            </>
          )}
          {format === "json" && <Row label=""><Checkbox label="Pretty print" checked={opts.jsonPretty} onChange={(v) => set("jsonPretty", v)} /></Row>}
          {format === "xlsx" && (
            <>
              <Row label="Sheet name"><Input value={opts.sheetName} onChange={(e) => set("sheetName", e.target.value)} placeholder="Sheet1" className="w-48" /></Row>
              <Row label=""><Checkbox label="Include header row" checked={opts.header} onChange={(v) => set("header", v)} /></Row>
            </>
          )}
          {format === "sql" && (
            <>
              <Row label="SQL dialect">
                <Select value={opts.targetDialect} onChange={(e) => set("targetDialect", e.target.value)}>
                  {DIALECTS.map(([v, l]) => <option key={v} value={v}>{l}</option>)}
                </Select>
              </Row>
              <Row label="">
                <div className="flex flex-wrap gap-x-4 gap-y-1">
                  <Checkbox label="Include CREATE TABLE" checked={opts.sqlIncludeCreate} onChange={(v) => set("sqlIncludeCreate", v)} />
                  <Checkbox label="Include DROP TABLE" checked={opts.sqlIncludeDrop} onChange={(v) => set("sqlIncludeDrop", v)} />
                </div>
              </Row>
              {!multi && (
                <Row label="Target table name" hint="Name used in INSERT statements (defaults to the source table).">
                  <Input value={opts.sqlTableName} onChange={(e) => set("sqlTableName", e.target.value)} placeholder={single ?? ""} className="font-mono text-xs" />
                </Row>
              )}
            </>
          )}
        </Section>
        {source === "tables" && !multi && chosen.length === 1 && (
          <Section title="Filter rows (optional)">
            <Input value={where} onChange={(e) => setWhere(e.target.value)} className="font-mono text-xs"
              placeholder={isMongo ? '{ "status": "active" }' : "WHERE expression, e.g. created_at > '2024-01-01'"} />
          </Section>
        )}
        <Section title="Output">
          <Row label={multi ? "Folder" : "File"} hint={multi ? `One file per table: <table>.${EXT[format]}` : undefined}>
            <PathField
              value={path}
              onChange={setPath}
              mode={multi ? "folder" : "save"}
              dirKey="export"
              defaultName={`${safeFileName(single ?? "export")}.${EXT[format]}`}
              filters={[{ name: FORMAT_LABEL[format], extensions: [EXT[format]] }]}
            />
          </Row>
          <div className="mt-3 flex justify-end">
            <Button variant="primary" onClick={run} disabled={!canRun} loading={job.job.running}><Play className="size-3.5" />Export</Button>
          </div>
        </Section>
      </div>
    </div>
  );
}
