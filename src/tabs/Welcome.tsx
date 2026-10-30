// The first tab: what DevCat does, as things to start. Agents first (a chat in a folder, a ghost
// chat), then what they work with (repos, connections, rooms…), then how to reach them from
// elsewhere (phone, chat apps, other agents over MCP, shared memory).
import { ArrowLeftRight, Bot, Brain, Cable, GitBranch, GitCompare, Ghost, ListTodo, NotebookPen, Plug, Shield, Smartphone, Users } from "lucide-react";
import type { ReactNode } from "react";
import { AiTerminalIcon } from "../components/ui/AppIcons";
import { KIND_META, KindIcon } from "../components/ui/KindIcon";
import { openGhostChat } from "../features/agent/ghost";
import { openClaudeWorkspace } from "../features/claude/open";
import { pickFolder, useWorkspace } from "../features/claude/sidebar/workspace";
import { openMcpSync } from "../features/connections/mcp/McpSyncDialog";
import { openIntegrations } from "../features/integrations/store";
import { openMemory } from "../features/memory/MemoryDialog";
import { useMobile } from "../features/mobile/store";
import { openSetup, startTour } from "../features/onboarding/store";
import type { ConnectionKind } from "../lib/types";
import { IS_MAC, NEW_WINDOW_SHORTCUT } from "../lib/windows";
import { openTab } from "../store/tabs";
import { useUi } from "../store/ui";

const kinds = Object.keys(KIND_META) as ConnectionKind[];

export default function Welcome() {
  const openDialog = useUi((s) => s.openConnectionDialog);
  const setSidebar = useUi((s) => s.setSidebar);
  const cwd = useWorkspace((s) => s.cwd);
  const folder = cwd?.split(/[\\/]/).pop();

  const newChat = async () => {
    setSidebar("claude");
    const dir = cwd ?? (await pickFolder());
    if (dir) openClaudeWorkspace(dir);
  };

  return (
    <div className="h-full overflow-auto">
      <div className="mx-auto max-w-4xl px-8 py-12">
        <div className="flex items-center gap-4">
          <img src="/icon.svg" alt="" className="size-14" />
          <div>
            <h1 className="text-3xl font-bold tracking-tight">DevCat</h1>
            <p className="mt-0.5 text-[15px] text-fg/90">Your agents, repos, databases and clusters. One window.</p>
            <p className="mt-0.5 text-[13px] text-muted">Run any coding agent, review what it did, build with your team in rooms, and keep it going from your phone or a chat app.</p>
            <p className="mt-1.5 text-[12.5px]">
              <button className="text-accent hover:underline" onClick={startTour}>Take the tour</button>
              <span className="mx-1.5 text-faint">·</span>
              <button className="text-accent hover:underline" onClick={openSetup}>Set up DevCat</button>
            </p>
          </div>
        </div>

        <Section title="Work with an agent">
          <div className="grid grid-cols-3 gap-2">
            <Card big icon={<AiTerminalIcon className="size-5 text-accent" />} title="New agent chat"
              desc={folder ? `Claude Code, Codex, Gemini or the built-in DevCat Agent, in ${folder}` : "Claude Code, Codex, Gemini or the built-in DevCat Agent, in a folder you pick"}
              onClick={() => void newChat()} />
            <Card big icon={<Ghost className="size-5 text-muted" />} title="Ghost chat"
              desc="A fresh DevCat Agent chat in its own window: no project, and nothing is kept when you close it"
              onClick={() => void openGhostChat()} />
            <Card big icon={<ListTodo className="size-5 text-info" />} title="Tickets"
              desc="Let the agent fix Jira, Trello or Linear tickets on their own branches"
              onClick={() => setSidebar("tickets")} />
          </div>
        </Section>

        <Section title="Everything it works with">
          <div className="grid grid-cols-3 gap-2">
            <Card icon={<GitBranch className="size-5 text-info" />} title="Git repositories" desc="Every repo in a folder: history, blame, branches, stashes"
              onClick={() => setSidebar("git")} />
            <Card icon={<Users className="size-5 text-accent" />} title="Rooms" desc="Work with other people, peer to peer: files, docs, whiteboards, tasks and their agents"
              onClick={() => setSidebar("rooms")} />
            <Card icon={<Shield className="size-5 text-ok" />} title="VPN & tunnels" desc="OpenVPN, WireGuard, SSH and Kubernetes forwards with live traffic"
              onClick={() => setSidebar("network")} />
            <Card icon={<NotebookPen className="size-5 text-warn" />} title="Notes" desc="Markdown notes per project, saved as you type"
              onClick={() => setSidebar("notes")} />
            <Card icon={<GitCompare className="size-5 text-info" />} title="Schema compare" desc="Diff two schemas and generate a sync script"
              onClick={() => openTab({ type: "schema-compare" }, "Schema Compare")} />
            <Card icon={<ArrowLeftRight className="size-5 text-ok" />} title="Data transfer" desc="Export, import, back up, restore, copy tables"
              onClick={() => openTab({ type: "data-transfer" }, "Data Transfer")} />
          </div>
        </Section>

        <Section title="Connect a database, cluster or server">
          <div className="grid grid-cols-5 gap-2">
            {kinds.map((k) => (
              <button
                key={k}
                onClick={() => openDialog(null, k)}
                className="flex items-center gap-2.5 rounded-lg border border-line bg-panel px-3 py-2.5 text-left hover:border-accent/60 hover:bg-raised"
              >
                <KindIcon kind={k} className="size-5" />
                <span className="truncate text-[13px]">{KIND_META[k].label}</span>
              </button>
            ))}
          </div>
          <p className="mt-2 text-xs text-faint">Right-click a connection → Ask DevCat Agent: a chat that already knows its tables or namespaces.</p>
        </Section>

        <Section title="Keep your agents going from anywhere">
          <div className="grid grid-cols-4 gap-2">
            <Card icon={<Smartphone className="size-5 text-accent" />} title="Mobile pairing" desc="Follow and answer your agents on your phone"
              onClick={() => useMobile.getState().show()} />
            <Card icon={<Bot className="size-5 text-info" />} title="Agent bots" desc="Approve, ask and get results in Telegram or Discord"
              onClick={openIntegrations} />
            <Card icon={<Cable className="size-5 text-ok" />} title="Sync MCP" desc="Your connections in Claude Code, Codex, Cursor and Gemini"
              onClick={openMcpSync} />
            <Card icon={<Brain className="size-5 text-warn" />} title="AI memory" desc="One memory every agent shares, across your computers"
              onClick={() => openMemory()} />
          </div>
        </Section>

        <p className="mt-10 flex items-center gap-1.5 text-xs text-faint">
          <Plug className="size-3.5" />
          Tips: {NEW_WINDOW_SHORTCUT} opens another window for another project · {IS_MAC ? "⌘P" : "Ctrl+P"} finds any file · the cup at the top right keeps your computer awake.
        </p>
      </div>
    </div>
  );
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <>
      <h2 className="mt-9 mb-3 text-xs font-semibold uppercase tracking-wider text-muted">{title}</h2>
      {children}
    </>
  );
}

function Card({ icon, title, desc, onClick, big }: { icon: ReactNode; title: string; desc: string; onClick: () => void; big?: boolean }) {
  return (
    <button onClick={onClick} className={`flex flex-col gap-2 rounded-lg border border-line bg-panel text-left hover:border-accent/60 hover:bg-raised ${big ? "p-4" : "p-3.5"}`}>
      {icon}
      <div className="font-medium">{title}</div>
      <div className="text-xs text-muted">{desc}</div>
    </button>
  );
}
