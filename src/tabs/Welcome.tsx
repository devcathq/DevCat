import { ArrowLeftRight, GitBranch, GitCompare, Shield } from "lucide-react";
import type { ReactNode } from "react";
import { AiTerminalIcon } from "../components/ui/AppIcons";
import { KIND_META, KindIcon } from "../components/ui/KindIcon";
import { openClaudeWorkspace } from "../features/claude/open";
import { pickFolder, useWorkspace } from "../features/claude/sidebar/workspace";
import type { ConnectionKind } from "../lib/types";
import { NEW_WINDOW_SHORTCUT } from "../lib/windows";
import { openTab } from "../store/tabs";
import { useUi } from "../store/ui";

const kinds = Object.keys(KIND_META) as ConnectionKind[];

export default function Welcome() {
  const openDialog = useUi((s) => s.openConnectionDialog);
  const setSidebar = useUi((s) => s.setSidebar);
  const cwd = useWorkspace((s) => s.cwd);

  const newChat = async () => {
    setSidebar("claude");
    const dir = cwd ?? (await pickFolder());
    if (dir) openClaudeWorkspace(dir);
  };

  return (
    <div className="h-full overflow-auto">
      <div className="mx-auto max-w-3xl px-8 py-14">
        <div className="flex items-center gap-4">
          <img src="/icon.svg" alt="" className="size-14" />
          <div>
            <h1 className="text-3xl font-bold tracking-tight">DevCat</h1>
            <p className="mt-0.5 text-muted">Your developer workbench: AI coding agents, Git, databases, clusters, queues and tunnels in one app.</p>
          </div>
        </div>

        <Section title="Start">
          <div className="grid grid-cols-3 gap-2">
            <Card icon={<AiTerminalIcon className="size-5 text-accent" />} title="AI chat"
              desc={cwd ? `Claude Code, Codex, Gemini… in ${cwd.split(/[\\/]/).pop()}` : "Claude Code, Codex, Gemini… in a folder you pick"}
              onClick={() => void newChat()} />
            <Card icon={<GitBranch className="size-5 text-info" />} title="Git repositories" desc="Find every repo in a folder: history, blame, branches"
              onClick={() => setSidebar("git")} />
            <Card icon={<Shield className="size-5 text-ok" />} title="VPN & SSH tunnels" desc="OpenVPN, WireGuard and SSH forwards with live traffic"
              onClick={() => setSidebar("network")} />
          </div>
        </Section>

        <Section title="New connection">
          <div className="grid grid-cols-4 gap-2">
            {kinds.map((k) => (
              <button
                key={k}
                onClick={() => openDialog(null, k)}
                className="flex items-center gap-2.5 rounded-lg border border-line bg-panel px-3 py-3 text-left hover:border-accent/60 hover:bg-raised"
              >
                <KindIcon kind={k} className="size-5" />
                <span className="text-[13px]">{KIND_META[k].label}</span>
              </button>
            ))}
          </div>
        </Section>

        <Section title="Database tools">
          <div className="grid grid-cols-3 gap-2">
            <Card icon={<GitCompare className="size-5 text-info" />} title="Schema compare" desc="Diff two schemas and generate a sync script"
              onClick={() => openTab({ type: "schema-compare" }, "Schema Compare")} />
            <Card icon={<ArrowLeftRight className="size-5 text-ok" />} title="Data transfer" desc="Export, import, backup, restore, copy tables"
              onClick={() => openTab({ type: "data-transfer" }, "Data Transfer")} />
          </div>
        </Section>

        <p className="mt-10 text-xs text-faint">
          Tip: {NEW_WINDOW_SHORTCUT} opens another window, so you can keep each project in its own. Settings (the gear at the top right) → Keep awake keeps your screen awake.
        </p>
      </div>
    </div>
  );
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <>
      <h2 className="mt-10 mb-3 text-xs font-semibold uppercase tracking-wider text-muted">{title}</h2>
      {children}
    </>
  );
}

function Card({ icon, title, desc, onClick }: { icon: ReactNode; title: string; desc: string; onClick: () => void }) {
  return (
    <button onClick={onClick} className="flex flex-col gap-2 rounded-lg border border-line bg-panel p-4 text-left hover:border-accent/60 hover:bg-raised">
      {icon}
      <div className="font-medium">{title}</div>
      <div className="text-xs text-muted">{desc}</div>
    </button>
  );
}
