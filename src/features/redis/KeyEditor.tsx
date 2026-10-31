// Right pane of the Redis tab: view/edit one key according to its type.
import Editor from "@monaco-editor/react";
import clsx from "clsx";
import { AlertTriangle, Braces, Clock, Copy, KeyRound, Plus, RefreshCw, Save, Trash2, Undo2, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import { Button, Dialog, Empty, ErrorBox, IconButton, Input, Spinner, Spin } from "../../components/ui";
import { api, errMsg } from "../../lib/api";
import type { RedisKeyValue } from "../../lib/types";
import { asJson, cmd, formatBytes, formatTtl, replyError, typeMeta } from "./util";

type Item = { a: string; b: string };
/** Editable working copy; `items` holds list/set members, zset [member, score], hash [field, value]. */
interface Draft { name: string; text: string; items: Item[] }

function toDraft(v: RedisKeyValue): Draft {
  const val = v.value;
  switch (v.type) {
    case "list": case "set": return { name: v.key, text: "", items: ((val as string[]) ?? []).map((a) => ({ a: String(a), b: "" })) };
    case "zset": return { name: v.key, text: "", items: ((val as [string, number][]) ?? []).map(([m, s]) => ({ a: String(m), b: String(s) })) };
    case "hash": return { name: v.key, text: "", items: Object.entries((val as Record<string, string>) ?? {}).map(([a, b]) => ({ a, b: String(b ?? "") })) };
    case "ReJSON-RL": return { name: v.key, text: JSON.stringify(val, null, 2), items: [] };
    default: return { name: v.key, text: typeof val === "string" ? val : val == null ? "" : JSON.stringify(val), items: [] };
  }
}

function fromDraft(type: string, d: Draft, originalText: string): unknown {
  switch (type) {
    case "list": case "set": return d.items.map((i) => i.a);
    case "zset": return d.items.map((i) => [i.a, Number(i.b)]);
    case "hash": return Object.fromEntries(d.items.map((i) => [i.a, i.b]));
    case "ReJSON-RL": return JSON.parse(d.text);
    default: {
      // Keep compact JSON compact when it was only pretty-printed for viewing.
      const orig = asJson(originalText);
      if (orig !== undefined && !originalText.includes("\n")) {
        const now = asJson(d.text);
        if (now !== undefined) return JSON.stringify(now);
      }
      return d.text;
    }
  }
}

export function KeyEditor({ connId, db, keyName, readOnly, onRenamed, onDeleted, onDirty }: {
  connId: string;
  db: string;
  keyName: string;
  readOnly?: boolean;
  onRenamed: (newKey: string) => void;
  onDeleted: () => void;
  onDirty?: (dirty: boolean) => void;
}) {
  const [data, setData] = useState<RedisKeyValue & { binary?: boolean } | null>(null);
  const [loadedAt, setLoadedAt] = useState(0);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [orig, setOrig] = useState<Draft | null>(null);
  const [saving, setSaving] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [ttlOpen, setTtlOpen] = useState(false);
  const [pretty, setPretty] = useState(true);
  const [sel, setSel] = useState<number | null>(null);
  const [filter, setFilter] = useState("");

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const v = (await api.getKey(connId, db, keyName)) as RedisKeyValue & { binary?: boolean };
      setData(v);
      setLoadedAt(Date.now());
      const d = toDraft(v);
      setDraft(d);
      setOrig(d);
      setSel(null);
    } catch (e) {
      setError(errMsg(e));
      setData(null);
    } finally {
      setLoading(false);
    }
  }, [connId, db, keyName]);
  useEffect(() => { load(); }, [load]);

  const type = data?.type ?? "none";
  const count = draft?.items.length ?? 0;
  const partial = !!data && typeof data.length === "number" && ["list", "set", "zset", "hash"].includes(type) && data.length > (orig?.items.length ?? 0);
  const editable = !readOnly && !!data && type !== "stream" && type !== "none" && !data.binary && !partial && !/not supported/.test(String(data.value));
  const dirty = !!draft && !!orig && JSON.stringify(draft) !== JSON.stringify(orig);
  const renamed = !!draft && !!data && draft.name !== data.key;
  const onDirtyRef = useRef(onDirty);
  onDirtyRef.current = onDirty;
  useEffect(() => { onDirtyRef.current?.(dirty); }, [dirty]);

  // Validation
  const problems = useMemo(() => {
    if (!draft) return [];
    const p: string[] = [];
    if (!draft.name) p.push("Key name is required.");
    if (type === "zset" && draft.items.some((i) => i.b.trim() === "" || Number.isNaN(Number(i.b)))) p.push("Every score must be a number.");
    if ((type === "hash" || type === "set" || type === "zset") && new Set(draft.items.map((i) => i.a)).size !== draft.items.length) p.push(type === "hash" ? "Duplicate field names." : "Duplicate members.");
    if (["list", "set", "zset", "hash"].includes(type) && !draft.items.length) p.push(`A ${type} cannot be empty — delete the key instead.`);
    if (type === "ReJSON-RL") { try { JSON.parse(draft.text); } catch { p.push("Invalid JSON."); } }
    return p;
  }, [draft, type]);

  const save = async () => {
    if (!draft || !data || problems.length) return;
    setSaving(true);
    try {
      const elapsed = Math.floor((Date.now() - loadedAt) / 1000);
      const ttl = data.ttl > 0 ? Math.max(1, data.ttl - elapsed) : -1;
      if (renamed && !dirtyValue()) {
        const rs = await api.execute(connId, cmd("RENAME", data.key, draft.name), { database: db });
        const err = replyError(rs);
        if (err) throw new Error(err);
      } else {
        await api.setKey(connId, db, {
          key: draft.name,
          ...(renamed ? { oldKey: data.key } : {}),
          type, ttl,
          value: fromDraft(type, draft, orig?.text ?? ""),
          length: draft.items.length || undefined,
        });
      }
      toast.success(renamed ? `Saved as ${draft.name}` : "Key saved");
      if (renamed) onRenamed(draft.name);
      else await load();
    } catch (e) {
      toast.error(`Save failed: ${errMsg(e)}`);
    } finally {
      setSaving(false);
    }
  };
  const dirtyValue = () => JSON.stringify({ ...draft, name: "" }) !== JSON.stringify({ ...orig, name: "" });

  const del = async () => {
    setConfirmDelete(false);
    try {
      await api.deleteKey(connId, db, keyName);
      toast.success(`Deleted ${keyName}`);
      onDeleted();
    } catch (e) {
      toast.error(`Delete failed: ${errMsg(e)}`);
    }
  };

  // Ctrl/Cmd+S to save
  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "s" && dirty && editable) {
        e.preventDefault();
        save();
      }
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  });

  if (loading && !data) return <div className="flex h-full items-center justify-center gap-2 text-muted"><Spinner />Loading key…</div>;
  if (error) return <ErrorBox error={error} />;
  if (!data || !draft) return null;
  if (type === "none") {
    return (
      <Empty icon={<KeyRound className="size-8" />} title="Key not found">
        <span className="font-mono">{keyName}</span> does not exist (it may have expired or been deleted).
        <div className="mt-3"><Button size="sm" onClick={onDeleted}>Close</Button></div>
      </Empty>
    );
  }

  const tm = typeMeta(type);
  const setItems = (items: Item[]) => setDraft({ ...draft, items });
  const jsonValue = type === "string" ? asJson(draft.text) : undefined;
  const isJson = jsonValue !== undefined;

  const shown = draft.items.map((it, i) => [it, i] as const).filter(([it]) => !filter || it.a.toLowerCase().includes(filter.toLowerCase()) || it.b.toLowerCase().includes(filter.toLowerCase()));

  return (
    <div className="flex h-full flex-col">
      {/* header */}
      <div className="flex shrink-0 items-center gap-2 border-b border-line bg-panel px-3 py-1.5">
        <span className={clsx("rounded px-1.5 font-mono text-[10.5px] font-semibold leading-5", tm.cls)}>{tm.label}</span>
        <Input
          value={draft.name}
          onChange={(e) => setDraft({ ...draft, name: e.target.value })}
          readOnly={readOnly}
          className={clsx("h-7 flex-1 font-mono text-[13px]", renamed && "border-warn")}
          title="Key name (edit to rename)"
        />
        <IconButton title="Copy key name" onClick={() => navigator.clipboard.writeText(data.key).then(() => toast.success("Copied"))}><Copy className="size-3.5" /></IconButton>
        <IconButton title="Reload" onClick={load}><Spin icon={RefreshCw} spinning={loading} className="size-3.5" /></IconButton>
        <IconButton title="Delete key" onClick={() => setConfirmDelete(true)} disabled={readOnly}><Trash2 className="size-3.5" /></IconButton>
      </div>
      <div className="flex shrink-0 items-center gap-3 border-b border-line bg-panel px-3 py-1 text-[11.5px] text-muted">
        <button className="flex items-center gap-1 hover:text-fg disabled:pointer-events-none" onClick={() => setTtlOpen(true)} disabled={readOnly} title="Edit TTL">
          <Clock className="size-3.5" />TTL: <span className={clsx(data.ttl > 0 ? "text-warn" : "text-fg/80")}>{formatTtl(data.ttl)}</span>
        </button>
        {typeof data.length === "number" && (
          <span>{type === "string" ? `Size: ${formatBytes(data.length)}` : `Length: ${data.length.toLocaleString()}`}</span>
        )}
        {type === "string" && isJson && (
          <button className={clsx("flex items-center gap-1 hover:text-fg", pretty && "text-accent")} onClick={() => setPretty((p) => !p)} title="Toggle JSON formatting">
            <Braces className="size-3.5" />JSON {pretty ? "formatted" : "raw"}
          </button>
        )}
        <div className="flex-1" />
        {problems.length > 0 && dirty && <span className="text-danger">{problems[0]}</span>}
        {dirty && <Button size="sm" variant="ghost" onClick={() => setDraft(orig)}><Undo2 className="size-3.5" />Revert</Button>}
        <Button size="sm" variant="primary" onClick={save} disabled={!dirty || !!problems.length || (!editable && !(renamed && !dirtyValue() && !readOnly))} loading={saving}>
          <Save className="size-3.5" />Save
        </Button>
      </div>
      {(partial || data.binary || readOnly) && (
        <div className="flex shrink-0 items-center gap-2 border-b border-line bg-warn/10 px-3 py-1 text-xs text-warn">
          <AlertTriangle className="size-3.5" />
          {readOnly ? "Connection is read-only." : data.binary ? "Binary value — shown lossy, editing disabled." :
            `Showing the first ${orig?.items.length.toLocaleString()} of ${data.length?.toLocaleString()} items. Editing is disabled for partially loaded keys; use the console.`}
        </div>
      )}

      {/* body */}
      <div className="min-h-0 flex-1">
        {(type === "string" || type === "ReJSON-RL" || !["list", "set", "zset", "hash", "stream"].includes(type)) && (
          <StringEditor
            value={draft.text}
            json={type === "ReJSON-RL" || (isJson && pretty)}
            readOnly={!editable}
            onChange={(text) => setDraft({ ...draft, text })}
          />
        )}
        {type === "stream" && <StreamView entries={(data.value as { id: string; fields: Record<string, string> }[]) ?? []} total={data.length} />}
        {["list", "set", "zset", "hash"].includes(type) && (
          <div className="flex h-full flex-col">
            <div className="flex shrink-0 items-center gap-2 border-b border-line px-2 py-1">
              <Input value={filter} onChange={(e) => setFilter(e.target.value)} placeholder="Filter items" className="h-6 w-56 text-xs" />
              <span className="text-[11px] text-faint">{shown.length === count ? `${count} items` : `${shown.length} of ${count}`}</span>
              <div className="flex-1" />
              <Button size="sm" disabled={!editable} onClick={() => {
                const it: Item = type === "zset" ? { a: "", b: "0" } : { a: "", b: "" };
                setItems([...draft.items, it]);
                setSel(draft.items.length);
                setFilter("");
              }}><Plus className="size-3.5" />Add {type === "hash" ? "field" : type === "list" ? "element" : "member"}</Button>
            </div>
            <div className="min-h-0 flex-1 overflow-auto">
              <table className="w-full border-collapse text-[12.5px]">
                <thead className="sticky top-0 z-10 bg-panel text-left text-[11px] text-muted">
                  <tr>
                    <th className="w-12 border-b border-line px-2 py-1 font-medium">#</th>
                    <th className="border-b border-line px-2 py-1 font-medium">{type === "hash" ? "Field" : type === "list" ? "Element" : "Member"}</th>
                    {(type === "hash" || type === "zset") && <th className={clsx("border-b border-line px-2 py-1 font-medium", type === "zset" && "w-36")}>{type === "hash" ? "Value" : "Score"}</th>}
                    <th className="w-8 border-b border-line" />
                  </tr>
                </thead>
                <tbody>
                  {shown.map(([it, i]) => (
                    <tr key={i} onClick={() => setSel(i)} className={clsx("group border-b border-line/40", sel === i ? "bg-accent/10" : "hover:bg-hover/60")}>
                      <td className="px-2 font-mono text-[11px] text-faint">{type === "list" ? i : i + 1}</td>
                      <td className="px-1 py-0.5">
                        <CellInput value={it.a} readOnly={!editable} onChange={(a) => setItems(draft.items.map((x, j) => (j === i ? { ...x, a } : x)))} />
                      </td>
                      {(type === "hash" || type === "zset") && (
                        <td className="px-1 py-0.5">
                          <CellInput value={it.b} readOnly={!editable} mono={type === "zset"}
                            invalid={type === "zset" && (it.b.trim() === "" || Number.isNaN(Number(it.b)))}
                            onChange={(b) => setItems(draft.items.map((x, j) => (j === i ? { ...x, b } : x)))} />
                        </td>
                      )}
                      <td className="text-center">
                        <IconButton title="Remove" disabled={!editable} className="opacity-0 group-hover:opacity-100"
                          onClick={(e) => { e.stopPropagation(); setItems(draft.items.filter((_, j) => j !== i)); setSel(null); }}>
                          <X className="size-3.5" />
                        </IconButton>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            {sel !== null && draft.items[sel] && (
              <div className="h-40 shrink-0 border-t border-line">
                <div className="flex h-6 items-center gap-2 border-b border-line bg-panel px-2 text-[11px] text-muted">
                  Value of {type === "hash" ? `field "${draft.items[sel].a}"` : `item ${sel}`}
                  <div className="flex-1" />
                  <IconButton title="Close" onClick={() => setSel(null)}><X className="size-3" /></IconButton>
                </div>
                <div className="h-[calc(100%-24px)]">
                  <StringEditor
                    value={type === "hash" ? draft.items[sel].b : draft.items[sel].a}
                    json={asJson(type === "hash" ? draft.items[sel].b : draft.items[sel].a) !== undefined}
                    readOnly={!editable}
                    compactJson
                    onChange={(v) => setItems(draft.items.map((x, j) => (j === sel ? (type === "hash" ? { ...x, b: v } : { ...x, a: v }) : x)))}
                  />
                </div>
              </div>
            )}
          </div>
        )}
      </div>

      <Dialog open={confirmDelete} onClose={() => setConfirmDelete(false)} title="Delete key?" width={420}
        footer={<><Button onClick={() => setConfirmDelete(false)}>Cancel</Button><Button variant="danger" onClick={del} autoFocus><Trash2 className="size-3.5" />Delete</Button></>}>
        <p className="text-[13px]">Permanently delete <span className="font-mono text-accent">{keyName}</span>?</p>
      </Dialog>
      {ttlOpen && <TtlDialog connId={connId} db={db} keyName={data.key} ttl={data.ttl} onClose={() => setTtlOpen(false)} onDone={load} />}
    </div>
  );
}

function CellInput({ value, onChange, readOnly, mono = true, invalid }: { value: string; onChange: (v: string) => void; readOnly?: boolean; mono?: boolean; invalid?: boolean }) {
  return (
    <input
      value={value}
      readOnly={readOnly}
      spellCheck={false}
      onChange={(e) => onChange(e.target.value)}
      className={clsx(
        "h-6 w-full rounded border border-transparent bg-transparent px-1.5 text-[12.5px] text-fg focus:border-accent focus:bg-app focus:outline-none",
        mono && "font-mono text-[12px]",
        invalid && "border-danger/60",
        !readOnly && "hover:border-line",
      )}
    />
  );
}

/** Monaco editor that shows JSON formatted while preserving the edited text. */
function StringEditor({ value, onChange, json, readOnly, compactJson }: { value: string; onChange: (v: string) => void; json: boolean; readOnly?: boolean; compactJson?: boolean }) {
  // When showing JSON formatted, edit the formatted text and convert back on change.
  const formatted = useMemo(() => {
    if (!json) return value;
    const v = asJson(value) ?? (() => { try { return JSON.parse(value); } catch { return undefined; } })();
    return v === undefined ? value : JSON.stringify(v, null, 2);
  }, [json, value]);
  const [text, setText] = useState(formatted);
  const lastEmitted = useRef(value);
  useEffect(() => {
    if (value !== lastEmitted.current) setText(formatted);
    lastEmitted.current = value;
  }, [value, formatted]);
  useEffect(() => { setText(formatted); /* toggled pretty */ }, [json]); // eslint-disable-line react-hooks/exhaustive-deps

  return (
    <Editor
      language={json ? "json" : "plaintext"}
      theme="devcat"
      value={text}
      onChange={(v) => {
        const t = v ?? "";
        setText(t);
        let out = t;
        if (json && compactJson && !value.includes("\n")) {
          try { out = JSON.stringify(JSON.parse(t)); } catch { /* keep as typed */ }
        }
        lastEmitted.current = out;
        onChange(out);
      }}
      options={{ readOnly, minimap: { enabled: false }, fontSize: 12.5, wordWrap: "on", scrollBeyondLastLine: false, automaticLayout: true, lineNumbersMinChars: 3 }}
    />
  );
}

function StreamView({ entries, total }: { entries: { id: string; fields: Record<string, string> }[]; total?: number }) {
  return (
    <div className="flex h-full flex-col">
      <div className="shrink-0 border-b border-line px-3 py-1 text-[11px] text-faint">
        Latest {entries.length.toLocaleString()} of {(total ?? entries.length).toLocaleString()} entries (read-only)
      </div>
      <div className="selectable min-h-0 flex-1 overflow-auto">
        <table className="w-full border-collapse text-[12px]">
          <thead className="sticky top-0 bg-panel text-left text-[11px] text-muted">
            <tr><th className="w-48 border-b border-line px-2 py-1 font-medium">ID</th><th className="border-b border-line px-2 py-1 font-medium">Fields</th></tr>
          </thead>
          <tbody>
            {entries.slice().reverse().map((e) => (
              <tr key={e.id} className="border-b border-line/40 align-top hover:bg-hover/60">
                <td className="px-2 py-1 font-mono text-[11.5px] text-accent">{e.id}</td>
                <td className="px-2 py-1 font-mono text-[11.5px]">
                  {Object.entries(e.fields).map(([k, v]) => (
                    <span key={k} className="mr-3 inline-block"><span className="text-muted">{k}</span>=<span className="text-fg">{v}</span></span>
                  ))}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function TtlDialog({ connId, db, keyName, ttl, onClose, onDone }: { connId: string; db: string; keyName: string; ttl: number; onClose: () => void; onDone: () => void }) {
  const [persist, setPersist] = useState(ttl < 0);
  const [secs, setSecs] = useState(ttl > 0 ? String(ttl) : "3600");
  const [busy, setBusy] = useState(false);
  const n = Number(secs);
  const valid = persist || (Number.isInteger(n) && n > 0);
  const apply = async () => {
    setBusy(true);
    try {
      const rs = await api.execute(connId, persist ? cmd("PERSIST", keyName) : cmd("EXPIRE", keyName, n), { database: db });
      const err = replyError(rs);
      if (err) throw new Error(err);
      toast.success(persist ? "Expiry removed" : `TTL set to ${formatTtl(n)}`);
      onDone();
      onClose();
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  };
  const presets = [60, 300, 3600, 86400, 604800];
  return (
    <Dialog open onClose={onClose} title="Set TTL" width={400}
      footer={<><Button onClick={onClose}>Cancel</Button><Button variant="primary" onClick={apply} disabled={!valid} loading={busy}>Apply</Button></>}>
      <div className="flex flex-col gap-3 text-[13px]">
        <label className="flex items-center gap-2"><input type="radio" checked={persist} onChange={() => setPersist(true)} className="accent-[var(--color-accent)]" />No expiry (PERSIST)</label>
        <label className="flex items-center gap-2">
          <input type="radio" checked={!persist} onChange={() => setPersist(false)} className="accent-[var(--color-accent)]" />Expire in
          <Input type="number" min={1} value={secs} onChange={(e) => { setSecs(e.target.value); setPersist(false); }} className="w-28" autoFocus onKeyDown={(e) => e.key === "Enter" && valid && apply()} />
          seconds
        </label>
        <div className="flex flex-wrap gap-1 pl-6">
          {presets.map((p) => <Button key={p} size="sm" variant="ghost" onClick={() => { setSecs(String(p)); setPersist(false); }}>{formatTtl(p)}</Button>)}
        </div>
      </div>
    </Dialog>
  );
}
