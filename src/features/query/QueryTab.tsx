// SQL / Mongo / Redis query editor with schema-aware completion and result grids.
import Editor, { type OnMount } from "@monaco-editor/react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Group, Panel, Separator } from "react-resizable-panels";
import { format as formatSql } from "sql-formatter";
import { toast } from "sonner";
import { ErrorBox } from "../../components/ui";
import { api, errMsg } from "../../lib/api";
import { monaco } from "../../lib/monaco";
import { dialectOf, type ConnectionKind } from "../../lib/types";
import { useConnections } from "../../store/connections";
import { useTabs } from "../../store/tabs";
import type { Tab } from "../../tabs/types";
import { confirmDialog, promptDialog } from "../connections/dialogs";
import { notifyHistoryChanged } from "../history/useHistoryData";
import { defaultSchema, formatterLanguage, isSqlKind } from "../table/sqlDialect";
import { invalidateCompletion, prefetchCompletion, registerCompletion, setModelContext } from "./completion";
import QueryToolbar, { type ExplainMode } from "./QueryToolbar";
import ResultsPanel, { type RunMessage, type RunState } from "./ResultsPanel";
import { splitScript, statementAt } from "./statements";
import { ensureOpen, useConnectionMeta } from "./useConnectionMeta";

type QueryTabT = Extract<Tab, { type: "query" }>;

const languageFor = (k?: ConnectionKind) => (k === "mongodb" ? "javascript" : k === "redis" ? "redis" : "sql");

const isRisky = (s: string) =>
  /^\s*(drop|truncate)\b/i.test(s) ||
  (/^\s*(delete|update)\b/i.test(s) && !/\bwhere\b/i.test(s)) ||
  /^\s*alter\b[\s\S]*\bdrop\b/i.test(s);
const WRITE = /^\s*(insert|update|delete|drop|truncate|alter|create|replace|merge|grant|revoke|rename)\b/i;

let runSeq = 0;

export default function QueryTab({ tab }: { tab: QueryTabT }) {
  const conn = useConnections((s) => s.connections.find((c) => c.id === tab.connId));
  const update = useTabs((s) => s.update);
  const kind = conn?.kind;
  const dialect = kind ? dialectOf(kind) : "none";
  const database = tab.database ?? null;
  const schema = tab.schema ?? null;
  const meta = useConnectionMeta(tab.connId, kind, database);

  const editorRef = useRef<Parameters<OnMount>[0] | null>(null);
  const decorations = useRef<string[]>([]);
  const baseline = useRef(tab.initialSql ?? "");
  const [running, setRunning] = useState(false);
  const [startedAt, setStartedAt] = useState<number | null>(null);
  const [run, setRun] = useState<RunState | null>(null);
  const [maxRows, setMaxRows] = useState<number | null>(1000);

  // Default database / schema once metadata is known.
  useEffect(() => {
    if (!kind) return;
    if (!database && meta.databases.length) {
      const preferred = conn?.database && meta.databases.includes(conn.database) ? conn.database : meta.databases[0];
      update(tab.id, { database: preferred });
    }
  }, [meta.databases]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => {
    if (!kind || schema || !meta.schemas.length) return;
    const ds = defaultSchema(kind);
    update(tab.id, { schema: ds && meta.schemas.includes(ds) ? ds : meta.schemas[0] });
  }, [meta.schemas]); // eslint-disable-line react-hooks/exhaustive-deps

  // Completion context for this editor's model.
  const modelPath = `query-${tab.id}`;
  const [modelUri, setModelUri] = useState<string | null>(null);
  useEffect(() => {
    if (!kind || !modelUri) return;
    const ctx = { connId: tab.connId, kind, database, schema };
    setModelContext(modelUri, ctx);
    prefetchCompletion(ctx);
    return () => setModelContext(modelUri, null);
  }, [tab.connId, kind, database, schema, modelUri]);

  // Persist editor content into the tab (debounced) + dirty flag.
  const persistTimer = useRef<number | undefined>(undefined);
  const onChange = (value: string | undefined) => {
    window.clearTimeout(persistTimer.current);
    persistTimer.current = window.setTimeout(() => {
      const v = value ?? "";
      update(tab.id, { initialSql: v, dirty: v !== baseline.current });
    }, 300);
  };
  useEffect(() => () => window.clearTimeout(persistTimer.current), []);

  const getText = () => editorRef.current?.getValue() ?? "";

  /** Script to run: selection, statement under cursor, or everything. */
  const pickScript = (all: boolean): string | null => {
    const ed = editorRef.current;
    const model = ed?.getModel();
    if (!ed || !model || !kind) return null;
    const sel = ed.getSelection();
    if (sel && !sel.isEmpty()) return model.getValueInRange(sel);
    const text = model.getValue();
    if (all) return text;
    const pos = ed.getPosition();
    const offset = pos ? model.getOffsetAt(pos) : 0;
    const stmt = statementAt(splitScript(text, kind), offset);
    if (!stmt) return null;
    flash(model.getPositionAt(stmt.start), model.getPositionAt(stmt.end));
    return stmt.text;
  };

  const flash = (start: { lineNumber: number; column: number }, end: { lineNumber: number; column: number }) => {
    const ed = editorRef.current;
    if (!ed) return;
    decorations.current = ed.deltaDecorations(decorations.current, [{
      range: new monaco.Range(start.lineNumber, start.column, end.lineNumber, end.column),
      options: { className: "bg-accent/15", isWholeLine: false },
    }]);
    window.setTimeout(() => { decorations.current = ed.deltaDecorations(decorations.current, []); }, 700);
  };

  const execute = useCallback(async (script: string, readOnly = false) => {
    if (!kind || !conn) return;
    const trimmed = script.trim();
    if (!trimmed) return;
    if (conn.readOnly && isSqlKind(kind) && splitScript(trimmed, kind).some((s) => WRITE.test(s.text))) {
      toast.error("This connection is read-only", { description: "Write statements are blocked." });
      return;
    }
    if (conn.color === "#ef4444" && isSqlKind(kind)) {
      const risky = splitScript(trimmed, kind).filter((s) => isRisky(s.text));
      if (risky.length) {
        const ok = await confirmDialog({
          title: "Run on production?",
          message: <>This connection is tagged <b className="text-danger">production</b> and the script contains destructive statements.</>,
          code: risky.map((s) => s.text).join(";\n\n"),
          confirmLabel: "Run anyway",
          danger: true,
        });
        if (!ok) return;
      }
    }
    const t0 = Date.now();
    setRunning(true);
    setStartedAt(t0);
    try {
      await ensureOpen(conn.id);
      const results = readOnly
        ? await api.executeReadOnly(conn.id, trimmed, { database, schema, maxRows })
        : await api.execute(conn.id, trimmed, { database, schema, maxRows });
      const messages: RunMessage[] = results.map((r) => ({
        statement: r.statement || trimmed,
        ok: true,
        elapsedMs: r.elapsedMs,
        rows: r.columns.length ? r.rows.length : undefined,
        rowsAffected: r.rowsAffected,
        truncated: r.truncated,
        message: r.message,
      }));
      setRun({ id: ++runSeq, results, messages, totalMs: Date.now() - t0 });
      if (isSqlKind(kind) && /\b(create|alter|drop|rename)\b/i.test(trimmed)) invalidateCompletion(conn.id);
    } catch (e) {
      const error = errMsg(e);
      setRun({ id: ++runSeq, results: [], messages: [{ statement: trimmed, ok: false, elapsedMs: Date.now() - t0, error }], error, totalMs: Date.now() - t0 });
    } finally {
      setRunning(false);
      setStartedAt(null);
      notifyHistoryChanged();
    }
  }, [kind, conn, database, schema, maxRows]);

  // A query an agent opened to run (`db_open_query`): the whole script, once, as the agent may
  // (`read-only`: on a read-only session). Cleared first so a remount doesn't run it again.
  const [editorReady, setEditorReady] = useState(false);
  useEffect(() => {
    if (!tab.autoRun || !editorReady || !conn || !kind) return;
    const readOnly = tab.autoRun === "read-only";
    update(tab.id, { autoRun: undefined });
    void execute(getText(), readOnly);
  }, [tab.autoRun, editorReady, conn, kind]); // eslint-disable-line react-hooks/exhaustive-deps

  const runCurrent = (all: boolean) => {
    if (running) return;
    const s = pickScript(all);
    if (s) void execute(s);
  };

  const explain = (mode: ExplainMode) => {
    if (running || !kind) return;
    const s = pickScript(false)?.trim().replace(/;\s*$/, "");
    if (!s) return;
    let q: string;
    if (kind === "mongodb") q = `${s}.explain(${mode === "analyze" ? '"executionStats"' : ""})`;
    else if (kind === "sqlite") q = `EXPLAIN QUERY PLAN ${s}`;
    else if (kind === "postgres") q = mode === "analyze" ? `EXPLAIN (ANALYZE, BUFFERS) ${s}` : `EXPLAIN ${s}`;
    else q = mode === "analyze" ? `EXPLAIN ANALYZE ${s}` : `EXPLAIN ${s}`;
    void execute(q);
  };

  const stop = async () => {
    try { await api.cancel(tab.connId); toast("Cancel requested"); } catch (e) { toast.error("Cancel failed", { description: errMsg(e) }); }
  };

  const formatCode = () => {
    const ed = editorRef.current;
    const model = ed?.getModel();
    if (!ed || !model || !kind || !isSqlKind(kind)) return;
    const sel = ed.getSelection();
    const useSel = sel && !sel.isEmpty();
    const src = useSel ? model.getValueInRange(sel) : model.getValue();
    try {
      const out = formatSql(src, { language: formatterLanguage(dialect), keywordCase: "upper", tabWidth: 2 });
      ed.pushUndoStop();
      ed.executeEdits("format", [{ range: useSel ? sel : model.getFullModelRange(), text: out }]);
      ed.pushUndoStop();
    } catch (e) {
      toast.error("Could not format", { description: errMsg(e) });
    }
  };

  const saveQuery = async (asNew = false) => {
    const text = getText();
    let name = tab.savedQueryId && !asNew ? tab.title : null;
    if (!name) {
      name = await promptDialog({ title: "Save query", label: "Name", defaultValue: asNew ? `${tab.title} copy` : "", placeholder: "e.g. Active users by month", confirmLabel: "Save" });
      if (!name) return;
    }
    try {
      const saved = await api.saveQuery({
        id: asNew ? "" : tab.savedQueryId ?? "",
        name,
        connectionId: tab.connId,
        database,
        query: text,
        updatedAt: new Date().toISOString(),
      });
      baseline.current = text;
      update(tab.id, { savedQueryId: saved.id, title: saved.name, initialSql: text, dirty: false });
      notifyHistoryChanged();
      toast.success(`Saved “${saved.name}”`);
    } catch (e) {
      toast.error("Save failed", { description: errMsg(e) });
    }
  };

  // Keep latest handlers reachable from Monaco actions registered once on mount.
  const handlers = useRef({ runCurrent, saveQuery, formatCode, explain });
  handlers.current = { runCurrent, saveQuery, formatCode, explain };

  const onMount: OnMount = (ed, m) => {
    editorRef.current = ed;
    setModelUri(ed.getModel()?.uri.toString() ?? null);
    setEditorReady(true);
    const K = m.KeyMod, C = m.KeyCode;
    ed.addAction({ id: "devcat.run", label: "Run statement", keybindings: [K.CtrlCmd | C.Enter], run: () => handlers.current.runCurrent(false) });
    ed.addAction({ id: "devcat.runAll", label: "Run all", keybindings: [K.CtrlCmd | K.Shift | C.Enter], run: () => handlers.current.runCurrent(true) });
    ed.addAction({ id: "devcat.save", label: "Save query", keybindings: [K.CtrlCmd | C.KeyS], run: () => void handlers.current.saveQuery() });
    ed.addAction({ id: "devcat.format", label: "Format SQL", keybindings: [K.Shift | K.Alt | C.KeyF], run: () => handlers.current.formatCode() });
    ed.addAction({ id: "devcat.explain", label: "Explain statement", keybindings: [K.CtrlCmd | C.KeyE], run: () => handlers.current.explain("plain") });
    ed.focus();
  };

  const changeConnection = (connId: string) => {
    const c = useConnections.getState().byId(connId);
    update(tab.id, { connId, database: null, schema: null, title: tab.savedQueryId ? tab.title : `Query – ${c?.name ?? "?"}` });
  };

  const editorOptions = useMemo(() => ({
    fontSize: 13,
    fontFamily: "JetBrains Mono, SF Mono, Menlo, monospace",
    minimap: { enabled: false },
    automaticLayout: true,
    scrollBeyondLastLine: false,
    wordBasedSuggestions: "off" as const,
    suggest: { showWords: false, preview: true },
    quickSuggestions: { other: true, comments: false, strings: false },
    tabSize: 2,
    fixedOverflowWidgets: true,
    renderLineHighlight: "line" as const,
    lineNumbersMinChars: 3,
    padding: { top: 6 },
  }), []);

  if (!conn) {
    return <ErrorBox error="This tab's connection no longer exists. Pick another connection or close the tab." />;
  }

  return (
    <div className="flex h-full flex-col">
      {conn.color && <div className="h-0.5 shrink-0" style={{ background: conn.color }} />}
      <QueryToolbar
        tab={tab}
        kind={conn.kind}
        databases={meta.databases}
        schemas={meta.schemas}
        metaError={meta.error}
        running={running}
        maxRows={maxRows}
        onMaxRows={setMaxRows}
        onConnection={changeConnection}
        onDatabase={(d) => update(tab.id, { database: d, schema: null })}
        onSchema={(s) => update(tab.id, { schema: s })}
        onRun={() => runCurrent(false)}
        onRunAll={() => runCurrent(true)}
        onStop={stop}
        onExplain={explain}
        onFormat={formatCode}
        onSave={() => void saveQuery()}
        onSaveAs={() => void saveQuery(true)}
      />
      <Group orientation="vertical" className="min-h-0 flex-1">
        <Panel defaultSize="52%" minSize="15%">
          <Editor
            theme="devcat"
            path={modelPath}
            language={languageFor(kind)}
            defaultValue={tab.initialSql ?? ""}
            beforeMount={(m) => registerCompletion(m)}
            onMount={onMount}
            onChange={onChange}
            options={editorOptions}
          />
        </Panel>
        <Separator className="h-px bg-line hover:bg-accent data-[separator=active]:bg-accent" />
        <Panel minSize="12%">
          <ResultsPanel run={run} running={running} startedAt={startedAt} connId={conn.id} database={database} dialect={dialect} />
        </Panel>
      </Group>
    </div>
  );
}
