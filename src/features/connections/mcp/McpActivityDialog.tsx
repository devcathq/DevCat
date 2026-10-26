// MCP activity: every tool call agents made through DevCat's MCP server (`devcat mcp`), newest
// first: which agent, which connection, what it ran, and whether DevCat refused it. Like the phone
// activity log in Remote access, it stays on this computer. Mounted once by ConnectionDialog.
import { Activity, RefreshCw, Trash2 } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { toast } from "sonner";
import { create } from "zustand";
import { Button, Dialog, Spinner } from "../../../components/ui";
import { api, errMsg } from "../../../lib/api";
import type { McpAuditEntry } from "../../../lib/types";

const useMcpActivity = create<{ open: boolean }>(() => ({ open: false }));

export const openMcpActivity = () => useMcpActivity.setState({ open: true });

export function McpActivityDialog() {
  const open = useMcpActivity((s) => s.open);
  return open ? <ActivityView onClose={() => useMcpActivity.setState({ open: false })} /> : null;
}

function ActivityView({ onClose }: { onClose: () => void }) {
  const [rows, setRows] = useState<McpAuditEntry[] | null>(null);
  const load = useCallback(() => api.mcpAudit(300).then(setRows).catch((e) => toast.error(errMsg(e))), []);
  useEffect(() => { void load(); }, [load]);

  const clear = async () => {
    try {
      await api.mcpAuditClear();
      setRows([]);
    } catch (e) {
      toast.error(errMsg(e));
    }
  };

  return (
    <Dialog
      open
      title={<span className="flex items-center gap-2"><Activity className="size-4" />MCP activity</span>}
      width={760}
      onClose={onClose}
      footer={
        <>
          <Button className="mr-auto" disabled={!rows?.length} onClick={() => void clear()}><Trash2 className="size-3.5" />Clear</Button>
          <Button onClick={onClose}>Close</Button>
        </>
      }
    >
      <div className="flex flex-col gap-2 text-[13px]">
        <div className="flex items-center text-[12px] text-muted">
          <span className="flex-1">
            Every tool call Claude Code, Codex, Cursor, Gemini and other agents made through DevCat's MCP server, newest first.
            Refused writes are listed too. The log stays on this computer.
          </span>
          <Button size="sm" variant="ghost" onClick={() => void load()}><RefreshCw className="size-3" />Refresh</Button>
        </div>
        {!rows ? <div className="flex justify-center py-6"><Spinner /></div> : rows.length === 0 ? (
          <div className="py-6 text-center text-[12px] text-faint">Nothing yet.</div>
        ) : (
          <div className="max-h-[60vh] divide-y divide-line overflow-auto rounded-md border border-line">
            {rows.map((r, i) => (
              <div key={i} className="flex items-start gap-3 px-3 py-1.5 text-[12px]">
                <span className={r.ok ? "text-ok" : "text-danger"}>{r.ok ? "✓" : "✕"}</span>
                <div className="min-w-0 flex-1">
                  <span className="font-mono text-fg">{r.tool}</span>
                  {r.detail && <span className="selectable ml-2 break-all text-muted">{r.detail}</span>}
                  {r.error && <div className="selectable break-all text-danger/90">{r.error}</div>}
                </div>
                <span className="shrink-0 text-right text-faint">
                  {r.client}{r.project ? ` · ${r.project}` : ""} · {new Date(r.at).toLocaleString()} · {r.ms} ms
                </span>
              </div>
            ))}
          </div>
        )}
      </div>
    </Dialog>
  );
}
