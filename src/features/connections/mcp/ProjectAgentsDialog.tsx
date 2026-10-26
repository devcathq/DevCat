// A project's code folders and MCP writes. Agents working in the folders (DevCat Agent, and
// Claude Code / Codex / Cursor / Gemini through Sync MCP) get the project's connections; a
// project without folders shares them with every folder. Mounted once by ConnectionDialog.
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { FolderOpen, X } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { create } from "zustand";
import { Button, Checkbox, Dialog, ErrorBox, IconButton } from "../../../components/ui";
import { api, errMsg } from "../../../lib/api";
import type { Project } from "../../../lib/types";
import { useConnections } from "../../../store/connections";
import { openMcpSync } from "./McpSyncDialog";

const useProjectAgents = create<{ project: Project | null }>(() => ({ project: null }));

export const openProjectAgents = (project: Project) => useProjectAgents.setState({ project });

export function ProjectAgentsDialog() {
  const project = useProjectAgents((s) => s.project);
  return project ? <AgentsView key={project.id} project={project} onClose={() => useProjectAgents.setState({ project: null })} /> : null;
}

function AgentsView({ project, onClose }: { project: Project; onClose: () => void }) {
  const [folders, setFolders] = useState<string[]>(project.folders ?? []);
  const [writes, setWrites] = useState(!!project.mcpWrites);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const add = async () => {
    const dir = await openDialog({ directory: true, multiple: true, defaultPath: folders[0] });
    const picked = (Array.isArray(dir) ? dir : dir ? [dir] : []).filter((d) => !folders.includes(d));
    if (picked.length) setFolders([...folders, ...picked]);
  };

  const save = async () => {
    setBusy(true);
    try {
      await useConnections.getState().saveProject({ ...project, folders, mcpWrites: writes });
      // the backend updates synced agents' configs; until the first sync, offer it
      const plan = await api.mcpSyncPreview().catch(() => null);
      if (plan && !plan.enabled && plan.agents.length) {
        toast.success(`Saved ${project.name}`, { action: { label: "Sync MCP…", onClick: openMcpSync } });
      } else {
        toast.success(`Saved ${project.name}`);
      }
      onClose();
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      open
      title={`Code folders & MCP · ${project.name}`}
      width={560}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} onClick={() => void save()}>Save</Button>
        </>
      }
    >
      <div className="flex flex-col gap-4 text-[13px]">
        <p className="text-fg/90">
          Agents working in these folders (Claude Code, Codex, Cursor, Gemini and DevCat Agent) get this project's connections,
          plus the connections in no project. Without folders, this project's connections are shared with every folder.
        </p>
        <div className="flex flex-col gap-1">
          {folders.length === 0 && <div className="text-xs text-muted">No folders: shared with every folder.</div>}
          {folders.map((f) => (
            <div key={f} className="flex items-center gap-2 rounded-md border border-line bg-app px-2 py-1">
              <FolderOpen className="size-3.5 shrink-0 text-muted" />
              <span className="selectable min-w-0 flex-1 truncate font-mono text-xs" title={f}>{f}</span>
              <IconButton title="Remove" onClick={() => setFolders(folders.filter((x) => x !== f))}><X className="size-3.5" /></IconButton>
            </div>
          ))}
          <div><Button size="sm" onClick={() => void add()}><FolderOpen className="size-3.5" />Add folder…</Button></div>
        </div>
        <div className="flex flex-col gap-1">
          <Checkbox label="Allow writes from MCP" checked={writes} onChange={setWrites} />
          <span className="pl-5 text-xs text-muted">
            Agents connected over MCP may change data (INSERT, UPDATE, DELETE, DDL, Redis writes) on this project's connections.
            Off: they only read. Read-only connections stay read-only either way.
          </span>
        </div>
        {error && <ErrorBox error={error} />}
      </div>
    </Dialog>
  );
}
