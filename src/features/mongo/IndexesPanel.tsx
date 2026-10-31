// Indexes + inferred field list for a collection, with create / drop.
import clsx from "clsx";
import { KeyRound, Plus, RefreshCw, Trash2 } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { toast } from "sonner";
import { Badge, Button, Checkbox, Dialog, ErrorBox, Field, IconButton, Input, Spinner, Spin } from "../../components/ui";
import { api, errMsg } from "../../lib/api";
import type { IndexDef, TableRef, TableStructure } from "../../lib/types";
import { collRef, validateDoc } from "./ejson";

export function IndexesPanel({ connId, table, readOnly }: { connId: string; table: TableRef; readOnly?: boolean }) {
  const [data, setData] = useState<TableStructure | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [create, setCreate] = useState(false);
  const [drop, setDrop] = useState<IndexDef | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try { setData(await api.tableStructure(connId, table)); } catch (e) { setError(errMsg(e)); } finally { setLoading(false); }
  }, [connId, table]);
  useEffect(() => { load(); }, [load]);

  const doDrop = async () => {
    if (!drop) return;
    const ix = drop;
    setDrop(null);
    try {
      await api.execute(connId, `${collRef(table.name)}.dropIndex(${JSON.stringify(ix.name)})`, { database: table.database });
      toast.success(`Dropped index ${ix.name}`);
      load();
    } catch (e) { toast.error(errMsg(e)); }
  };

  return (
    <div className="h-full overflow-auto p-3">
      <div className="mb-2 flex items-center gap-2">
        <span className="text-[11px] font-semibold uppercase tracking-wider text-muted">Indexes</span>
        {data && <Badge>{data.indexes.length}</Badge>}
        <div className="flex-1" />
        <Button size="sm" onClick={() => setCreate(true)} disabled={readOnly}><Plus className="size-3.5" />Create index</Button>
        <IconButton title="Reload" onClick={load}><Spin icon={RefreshCw} spinning={loading} className="size-3.5" /></IconButton>
      </div>
      {error && <ErrorBox error={error} />}
      {loading && !data && <div className="flex items-center gap-2 text-xs text-muted"><Spinner className="size-3.5" />Loading…</div>}
      {data && (
        <>
          <div className="overflow-hidden rounded-md border border-line">
            <table className="selectable w-full text-[12.5px]">
              <thead className="bg-panel text-left text-[11px] text-muted">
                <tr><th className="px-2 py-1 font-medium">Name</th><th className="px-2 py-1 font-medium">Keys</th><th className="px-2 py-1 font-medium">Properties</th><th className="w-8" /></tr>
              </thead>
              <tbody>
                {data.indexes.map((ix) => (
                  <tr key={ix.name} className="group border-t border-line/50">
                    <td className="px-2 py-1 font-mono text-xs">
                      <span className="inline-flex items-center gap-1">{ix.primary && <KeyRound className="size-3 text-warn" />}{ix.name}</span>
                    </td>
                    <td className="px-2 py-1 font-mono text-xs text-muted">{ix.columns.join(", ")}</td>
                    <td className="px-2 py-1">
                      <span className="flex gap-1">
                        {ix.primary && <Badge tone="warn">primary</Badge>}
                        {ix.unique && <Badge tone="accent">unique</Badge>}
                        {ix.method && <Badge tone="info">{ix.method}</Badge>}
                      </span>
                    </td>
                    <td className="text-center">
                      {!ix.primary && ix.name !== "_id_" && (
                        <IconButton title="Drop index" disabled={readOnly} className="opacity-0 group-hover:opacity-100 hover:text-danger" onClick={() => setDrop(ix)}><Trash2 className="size-3.5" /></IconButton>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>

          <div className="mb-2 mt-5 flex items-center gap-2">
            <span className="text-[11px] font-semibold uppercase tracking-wider text-muted">Fields</span>
            <Badge>{data.columns.length}</Badge>
            {data.comment && <span className="text-[11px] text-faint">{data.comment}</span>}
          </div>
          <div className="overflow-hidden rounded-md border border-line">
            <table className="selectable w-full text-[12.5px]">
              <thead className="bg-panel text-left text-[11px] text-muted">
                <tr><th className="px-2 py-1 font-medium">Field</th><th className="px-2 py-1 font-medium">Types</th><th className="px-2 py-1 font-medium">Presence</th></tr>
              </thead>
              <tbody>
                {data.columns.map((c) => (
                  <tr key={c.name} className="border-t border-line/50">
                    <td className="px-2 py-1 font-mono text-xs">{c.name}</td>
                    <td className="px-2 py-1 font-mono text-xs text-info">{c.dataType}</td>
                    <td className="px-2 py-1 text-xs text-muted">{c.nullable ? "optional / nullable" : "always present"}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </>
      )}
      {create && <CreateIndexDialog connId={connId} table={table} onClose={() => setCreate(false)} onDone={() => { setCreate(false); load(); }} />}
      <Dialog open={!!drop} onClose={() => setDrop(null)} title="Drop index?" width={400}
        footer={<><Button onClick={() => setDrop(null)}>Cancel</Button><Button variant="danger" onClick={doDrop} autoFocus>Drop</Button></>}>
        <p className="text-[13px]">Drop index <span className="font-mono text-accent">{drop?.name}</span> on <span className="font-mono">{table.name}</span>?</p>
      </Dialog>
    </div>
  );
}

function CreateIndexDialog({ connId, table, onClose, onDone }: { connId: string; table: TableRef; onClose: () => void; onDone: () => void }) {
  const [keys, setKeys] = useState('{ "field": 1 }');
  const [name, setName] = useState("");
  const [unique, setUnique] = useState(false);
  const [sparse, setSparse] = useState(false);
  const [ttl, setTtl] = useState("");
  const [busy, setBusy] = useState(false);
  const err = validateDoc(keys) ?? (keys.trim() ? null : "Keys are required");
  const run = async () => {
    setBusy(true);
    try {
      const opts: Record<string, unknown> = {};
      if (name) opts.name = name;
      if (unique) opts.unique = true;
      if (sparse) opts.sparse = true;
      if (ttl) opts.expireAfterSeconds = Number(ttl);
      await api.execute(connId, `${collRef(table.name)}.createIndex(${keys}, ${JSON.stringify(opts)})`, { database: table.database });
      toast.success("Index created");
      onDone();
    } catch (e) { toast.error(errMsg(e)); } finally { setBusy(false); }
  };
  return (
    <Dialog open onClose={onClose} title={`Create index on ${table.name}`} width={480}
      footer={<><Button onClick={onClose}>Cancel</Button><Button variant="primary" onClick={run} disabled={!!err} loading={busy}>Create</Button></>}>
      <div className="flex flex-col gap-3">
        <Field label="Keys" hint={err ?? '1 = ascending, -1 = descending, "text", "2dsphere", "hashed"'}>
          <Input value={keys} onChange={(e) => setKeys(e.target.value)} className={clsx("font-mono text-xs", err && "border-danger")} autoFocus />
        </Field>
        <Field label="Name (optional)"><Input value={name} onChange={(e) => setName(e.target.value)} className="font-mono text-xs" /></Field>
        <div className="flex gap-5">
          <Checkbox label="Unique" checked={unique} onChange={setUnique} />
          <Checkbox label="Sparse" checked={sparse} onChange={setSparse} />
        </div>
        <Field label="TTL — expire after seconds (optional)"><Input type="number" min={0} value={ttl} onChange={(e) => setTtl(e.target.value)} className="w-40" /></Field>
      </div>
    </Dialog>
  );
}
