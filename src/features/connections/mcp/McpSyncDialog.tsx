// Sync MCP: puts DevCat's MCP server (`devcat mcp`) into Claude Code, Codex, Cursor and Gemini,
// so they use the user's connections without any setup. The dialog previews what changes in each
// agent's config (DevCat's own entry is added or replaced, never duplicated) and offers to replace
// other MCP servers that reach the same databases. Mounted once by ConnectionDialog.
import { Cable } from "lucide-react";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { create } from "zustand";
import { Badge, Button, Checkbox, Dialog, ErrorBox, Spinner } from "../../../components/ui";
import { api, errMsg } from "../../../lib/api";
import type { McpSyncPlan } from "../../../lib/types";
import { useConnections } from "../../../store/connections";
import { openMcpActivity } from "./McpActivityDialog";

const useMcpSync = create<{ open: boolean }>(() => ({ open: false }));

export const openMcpSync = () => useMcpSync.setState({ open: true });

const ACTION: Record<McpSyncPlan["changes"][number]["action"], { label: string; tone: "ok" | "info" | "warn" | "default" }> = {
  add: { label: "Add", tone: "ok" },
  update: { label: "Replace", tone: "info" },
  remove: { label: "Remove", tone: "warn" },
  unchanged: { label: "Up to date", tone: "default" },
};

/** "2 added, 1 replaced" for the toast. */
function summary(plan: McpSyncPlan, replaced: number): string {
  const n = (a: string) => plan.changes.filter((c) => c.action === a).length;
  const parts = [
    n("add") && `${n("add")} added`,
    n("update") && `${n("update")} replaced`,
    n("remove") && `${n("remove")} removed`,
    replaced && `${replaced} other server${replaced === 1 ? "" : "s"} replaced`,
  ].filter(Boolean);
  return parts.length ? parts.join(", ") : "already up to date";
}

export function McpSyncDialog() {
  const open = useMcpSync((s) => s.open);
  return open ? <SyncView onClose={() => useMcpSync.setState({ open: false })} /> : null;
}

function SyncView({ onClose }: { onClose: () => void }) {
  const [plan, setPlan] = useState<McpSyncPlan | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [replace, setReplace] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const projects = useConnections((s) => s.projects);
  const noFolders = projects.length > 0 && projects.every((p) => !p.folders?.length);

  useEffect(() => {
    api.mcpSyncPreview().then(setPlan, (e) => setError(errMsg(e)));
  }, []);

  const apply = async () => {
    setBusy(true);
    try {
      const done = await api.mcpSyncApply([...replace]);
      toast.success(`MCP synced: ${summary(done, replace.size)}`, {
        description: "Start a new agent session to pick it up. DevCat keeps it up to date as your projects change.",
      });
      onClose();
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setBusy(false);
    }
  };

  const tick = (id: string, on: boolean) => setReplace((s) => {
    const next = new Set(s);
    if (on) next.add(id); else next.delete(id);
    return next;
  });

  return (
    <Dialog
      open
      title={<span className="flex items-center gap-2"><Cable className="size-4" />Sync MCP</span>}
      width={720}
      onClose={onClose}
      footer={
        <>
          <Button className="mr-auto" onClick={() => { onClose(); openMcpActivity(); }}>Activity…</Button>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} disabled={!plan || !plan.agents.length} onClick={() => void apply()}>Sync</Button>
        </>
      }
    >
      <div className="flex flex-col gap-4 text-[13px]">
        <p className="text-fg/90">
          Adds DevCat's MCP server to your coding agents, so they can read schemas and run queries on your connections
          without any setup. In a project's code folder an agent gets that project's connections plus the shared ones;
          anywhere else, only the shared ones. Passwords stay in DevCat. Each connection's AI / MCP access (Edit connection →
          Advanced) says whether agents see it, only read, or may change data; new connections start read-only.
        </p>
        {error && <ErrorBox error={error} />}
        {!plan && !error && <div className="flex justify-center py-6"><Spinner /></div>}
        {plan && (
          <>
            {plan.agents.length > 0 && (
              <div className="flex flex-wrap items-center gap-1.5">
                <span className="text-xs text-muted">Found:</span>
                {plan.agents.map((a) => <Badge key={a} tone="accent">{a}</Badge>)}
              </div>
            )}
            {plan.changes.length > 0 && (
              <table className="w-full text-left text-xs">
                <thead className="text-muted">
                  <tr><th className="pb-1 font-normal">Agent</th><th className="pb-1 font-normal">For</th><th className="pb-1 font-normal">File</th><th /></tr>
                </thead>
                <tbody>
                  {plan.changes.map((c, i) => (
                    <tr key={i} className="border-t border-line/60">
                      <td className="py-1.5 pr-3 whitespace-nowrap">{c.agent}</td>
                      <td className="selectable py-1.5 pr-3 break-all">{c.scope}</td>
                      <td className="selectable py-1.5 pr-3 font-mono text-[11px] text-muted break-all">{c.file}</td>
                      <td className="py-1.5 text-right"><Badge tone={ACTION[c.action].tone}>{ACTION[c.action].label}</Badge></td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
            {plan.lookalikes.length > 0 && (
              <div className="flex flex-col gap-2">
                <div className="font-medium">Other MCP servers for your connections</div>
                <div className="text-xs text-muted">
                  These servers reach a database DevCat already has. Tick the ones DevCat should replace: they're removed from that config,
                  and the agent uses DevCat's server instead. Unticked ones are left as they are.
                </div>
                {plan.lookalikes.map((l) => (
                  <div key={l.id} className="flex flex-col gap-0.5">
                    <Checkbox
                      label={`Replace "${l.server}" (${l.agent}, ${l.scope}): reaches ${l.connection}`}
                      checked={replace.has(l.id)}
                      onChange={(on) => tick(l.id, on)}
                    />
                    <span className="selectable pl-5 font-mono text-[11px] text-faint break-all">{l.file}</span>
                  </div>
                ))}
              </div>
            )}
            {plan.notes.map((n) => <div key={n} className="rounded-md border border-warn/30 bg-warn/10 p-2 text-xs">{n}</div>)}
            {noFolders && (
              <div className="text-xs text-muted">
                Tip: give a project its code folders (right-click the project → Code folders &amp; MCP…) to keep its connections to that repository.
                Until then every agent gets every connection.
              </div>
            )}
          </>
        )}
      </div>
    </Dialog>
  );
}

let suggested = false;

/** Once: offer to set it up, when there are connections and an agent to give them to. */
export async function suggestMcpSync() {
  if (suggested) return;
  suggested = true;
  try {
    const plan = await api.mcpSyncPreview();
    if (plan.enabled || plan.dismissed || !plan.agents.length || !useConnections.getState().connections.length) return;
    toast(`Let ${plan.agents.join(", ")} use your connections?`, {
      description: "Sync MCP adds DevCat's MCP server to them: agents can then read schemas and run queries without setup.",
      duration: Infinity,
      action: { label: "Set up…", onClick: openMcpSync },
      cancel: { label: "Not now", onClick: () => void api.mcpSyncDismiss().catch(() => {}) },
    });
  } catch {
    // older backend or no home folder: nothing to offer
  }
}
