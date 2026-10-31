// The setup guide on a fresh install: pick the coding agent (what's installed is checked, with how to
// install the rest, or set up the built-in DevCat Agent), a project folder, maybe a connection, and
// what else is there (phone, chat apps, MCP, memory). Every step can be skipped, and the whole guide
// too; it opens again from Settings → Set up DevCat.
import clsx from "clsx";
import { Bot, Brain, Cable, Check, CheckCircle2, Circle, Copy, FolderOpen, Smartphone } from "lucide-react";
import { useEffect, useState, type ReactNode } from "react";
import { toast } from "sonner";
import { Button, Spinner } from "../../components/ui";
import { AiTerminalIcon } from "../../components/ui/AppIcons";
import { KIND_META, KindIcon } from "../../components/ui/KindIcon";
import { errMsg } from "../../lib/api";
import type { AiKind, ConnectionKind } from "../../lib/types";
import { useConnections } from "../../store/connections";
import { useUi } from "../../store/ui";
import { openClaudeWorkspace } from "../claude/open";
import { CLAUDE_PROVIDER_ID, KINDS, KIND_ORDER, blankProvider, detectProviders, loadProviders, openManageProviders, saveProvider, selectProvider, useProviders } from "../claude/providers";
import { pickFolder, useWorkspace } from "../claude/sidebar/workspace";
import { openMcpSync } from "../connections/mcp/McpSyncDialog";
import { openIntegrations } from "../integrations/store";
import { openMemory } from "../memory/MemoryDialog";
import { useMobile } from "../mobile/store";
import { finishSetup, useOnboarding } from "./store";

const STEPS = ["Welcome", "Your agent", "Your folder", "Connect", "From anywhere", "Ready"];
const AGENT_KINDS = KIND_ORDER.filter((k) => k !== "custom");
const CONN_KINDS = Object.keys(KIND_META) as ConnectionKind[];

export function Setup() {
  const open = useOnboarding((s) => s.setup);
  const [step, setStep] = useState(0);
  useEffect(() => { if (open) { setStep(0); void loadProviders(); void detectProviders(true); } }, [open]);
  if (!open) return null;
  const next = () => setStep((s) => Math.min(s + 1, STEPS.length - 1));
  const back = () => setStep((s) => Math.max(s - 1, 0));

  return (
    // under the dialogs it opens (they start at z-50)
    <div className="fixed inset-0 z-40 flex flex-col bg-app">
      <div className="flex h-12 shrink-0 items-center gap-3 border-b border-line px-5">
        <img src="/icon.svg" alt="" className="size-6" />
        <span className="font-semibold">Set up DevCat</span>
        <div className="ml-6 flex items-center gap-1.5">
          {STEPS.map((s, n) => (
            <button key={s} onClick={() => setStep(n)} className={clsx("flex items-center gap-1.5 rounded-full px-2.5 py-1 text-[12px]", n === step ? "bg-accent/15 text-accent" : n < step ? "text-fg/80 hover:bg-hover" : "text-faint hover:bg-hover")}>
              {n < step ? <Check className="size-3" /> : <span className="text-[10px] tabular-nums">{n + 1}</span>}{s}
            </button>
          ))}
        </div>
        <div className="flex-1" />
        <button className="text-[12.5px] text-muted hover:text-fg" onClick={() => finishSetup()}>Skip setup</button>
      </div>

      <div className="min-h-0 flex-1 overflow-auto">
        <div className="mx-auto max-w-3xl px-8 py-12">
          {step === 0 && <WelcomeStep onNext={next} />}
          {step === 1 && <AgentStep onNext={next} />}
          {step === 2 && <FolderStep />}
          {step === 3 && <ConnectStep />}
          {step === 4 && <AnywhereStep />}
          {step === 5 && <ReadyStep />}
        </div>
      </div>

      {step > 0 && step < 5 && (
        <div className="flex h-14 shrink-0 items-center gap-2 border-t border-line px-5">
          <Button variant="ghost" onClick={back}>Back</Button>
          <div className="flex-1" />
          {step !== 1 && <Button variant="ghost" onClick={next}>Skip this step</Button>}
          {step !== 1 && <Button variant="primary" onClick={next}>Continue</Button>}
        </div>
      )}
    </div>
  );
}

function Title({ kicker, title, children }: { kicker: string; title: string; children?: ReactNode }) {
  return (
    <div className="mb-7">
      <div className="text-[11px] font-semibold tracking-[.14em] text-accent uppercase">{kicker}</div>
      <h1 className="mt-1.5 text-2xl font-bold tracking-tight">{title}</h1>
      {children && <p className="mt-2 text-[13.5px] leading-relaxed text-muted">{children}</p>}
    </div>
  );
}

function WelcomeStep({ onNext }: { onNext: () => void }) {
  return (
    <div className="flex flex-col items-center pt-10 text-center">
      <img src="/icon.svg" alt="" className="size-20" />
      <h1 className="mt-6 text-4xl font-bold tracking-tight">Welcome to DevCat</h1>
      <p className="mt-3 text-[17px] text-fg/90">Your agents, repos, databases and clusters. One window.</p>
      <p className="mt-2 max-w-lg text-[13.5px] text-muted">A minute to set it up: pick the coding agent you use, a project folder, and anything you'd like to connect. You can skip any of it and change it all later.</p>
      <div className="mt-8 flex gap-2">
        <Button variant="primary" onClick={onNext}>Get started</Button>
        <Button variant="ghost" onClick={() => finishSetup()}>Skip setup</Button>
      </div>
      <p className="mt-10 text-[11.5px] text-faint">Everything stays on this computer: no account, and passwords go in your system keychain.</p>
    </div>
  );
}

function CopyCode({ code }: { code: string }) {
  return (
    <span className="mt-1.5 flex items-center gap-2 rounded-md border border-line bg-app px-2 py-1 font-mono text-[11.5px] text-fg/90">
      <span className="min-w-0 flex-1 truncate">{code}</span>
      <button title="Copy" className="text-faint hover:text-fg" onClick={(e) => { e.stopPropagation(); void navigator.clipboard.writeText(code).then(() => toast.success("Copied")); }}><Copy className="size-3.5" /></button>
    </span>
  );
}

function AgentStep({ onNext }: { onNext: () => void }) {
  const providers = useProviders((s) => s.providers);
  const detect = useProviders((s) => s.detect);
  const detecting = useProviders((s) => s.detecting);
  const selectedId = useProviders((s) => s.selectedId);
  const current = providers.find((p) => p.id === selectedId)?.kind ?? "claude";
  const [pick, setPick] = useState<AiKind>(current);
  const [busy, setBusy] = useState(false);
  const agentProvider = providers.find((p) => p.kind === "agent");

  const status = (k: AiKind): { ok: boolean; text: string } => {
    if (k === "agent") return agentProvider ? { ok: true, text: `Set up: ${agentProvider.name}` } : { ok: false, text: "Built in: needs a model API (Anthropic, OpenAI, DeepSeek, GLM, Ollama…)" };
    const d = detect[k];
    if (!d) return { ok: false, text: detecting ? "Checking…" : Object.keys(detect).length ? "Not installed" : "Not checked" };
    return d.path ? { ok: true, text: `Installed${d.version ? ` · ${d.version}` : ""}` } : { ok: false, text: "Not installed" };
  };

  const use = async () => {
    setBusy(true);
    try {
      if (pick === "claude") selectProvider(CLAUDE_PROVIDER_ID);
      else if (pick === "agent") {
        if (!agentProvider) { openManageProviders("", "agent"); return; }
        selectProvider(agentProvider.id);
      } else {
        const existing = providers.find((p) => p.kind === pick);
        selectProvider((existing ?? (await saveProvider(blankProvider(pick)))).id);
      }
      onNext();
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  };

  const s = status(pick);
  return (
    <>
      <Title kicker="Step 1" title="Which coding agent do you use?">
        DevCat runs the agent you already have, in a real terminal, with its own login. Or use the built-in DevCat Agent with any model API. You can switch any time, per chat.
      </Title>
      <div className="grid grid-cols-2 gap-2">
        {AGENT_KINDS.map((k) => {
          const st = status(k);
          return (
            <button key={k} onClick={() => setPick(k)}
              className={clsx("flex items-start gap-3 rounded-lg border p-3.5 text-left", pick === k ? "border-accent bg-accent/5" : "border-line bg-panel hover:border-accent/50")}>
              {pick === k ? <CheckCircle2 className="mt-0.5 size-4 shrink-0 text-accent" /> : <Circle className="mt-0.5 size-4 shrink-0 text-faint" />}
              <div className="min-w-0">
                <div className="flex items-center gap-2 font-medium">{k === "agent" ? <Bot className="size-4 text-accent" /> : <AiTerminalIcon className="size-4 text-muted" />}{KINDS[k].label}{k === "claude" && <span className="text-[10.5px] text-faint">default</span>}</div>
                <div className={clsx("mt-0.5 text-[12px]", st.ok ? "text-ok" : "text-muted")}>{st.text}</div>
              </div>
            </button>
          );
        })}
      </div>

      <div className="mt-4 rounded-lg border border-line bg-panel p-4 text-[13px]">
        {pick === "agent" ? (
          agentProvider ? <p className="text-muted">DevCat Agent is set up with <b className="text-fg">{agentProvider.name}</b>.</p> : (
            <>
              <p className="text-muted">Nothing to install. Pick a model API and paste its key; it's kept in your system keychain.</p>
              <Button className="mt-3" variant="primary" onClick={() => openManageProviders("", "agent")}>Set up DevCat Agent…</Button>
            </>
          )
        ) : s.ok ? (
          <p className="text-muted"><b className="text-fg">{KINDS[pick].label}</b> is ready. {KINDS[pick].login}</p>
        ) : (
          <>
            <p className="text-muted">Install <b className="text-fg">{KINDS[pick].label}</b> in a terminal, then check again. {KINDS[pick].login}</p>
            {KINDS[pick].install && <CopyCode code={KINDS[pick].install!} />}
            {pick === "claude" && <><div className="mt-2 text-[11px] text-faint">Windows (PowerShell)</div><CopyCode code="irm https://claude.ai/install.ps1 | iex" /></>}
            <Button className="mt-3" size="sm" loading={detecting} onClick={() => void detectProviders(true)}>Check again</Button>
          </>
        )}
      </div>

      <div className="mt-6 flex items-center gap-2">
        <div className="flex-1" />
        <Button variant="ghost" onClick={onNext}>Skip this step</Button>
        <Button variant="primary" loading={busy} disabled={pick === "agent" && !agentProvider} onClick={() => void use()}>
          Use {KINDS[pick].label}
        </Button>
      </div>
    </>
  );
}

function FolderStep() {
  const cwd = useWorkspace((s) => s.cwd);
  return (
    <>
      <Title kicker="Step 2" title="Pick a project folder">
        The agent works in a folder: it reads its code and rules (AGENTS.md, CLAUDE.md), and you see its files, git status and past chats beside the chat. You can open other folders any time.
      </Title>
      <div className="flex items-center gap-3 rounded-lg border border-line bg-panel p-4">
        <FolderOpen className="size-5 text-accent" />
        <div className="min-w-0 flex-1">
          <div className="font-medium">{cwd ? cwd.split(/[\\/]/).pop() : "No folder yet"}</div>
          <div className="truncate font-mono text-[11.5px] text-muted">{cwd ?? "Choose the folder of a project you work on"}</div>
        </div>
        <Button variant={cwd ? "default" : "primary"} onClick={() => void pickFolder()}>{cwd ? "Change…" : "Choose folder…"}</Button>
      </div>
      <p className="mt-3 text-[12px] text-faint">Folders on a server work too: AI workspace → Folder on SSH host.</p>
    </>
  );
}

function ConnectStep() {
  const openDialog = useUi((s) => s.openConnectionDialog);
  const conns = useConnections((s) => s.connections);
  return (
    <>
      <Title kicker="Step 3 · optional" title="Connect a database, cluster or server">
        Browse and edit data, run queries, check pods and logs, and let agents use them too (read-only unless you allow more). Passwords go in your system keychain.
      </Title>
      <div className="grid grid-cols-5 gap-2">
        {CONN_KINDS.map((k) => (
          <button key={k} onClick={() => openDialog(null, k)} className="flex items-center gap-2 rounded-lg border border-line bg-panel px-3 py-2.5 text-left hover:border-accent/60 hover:bg-raised">
            <KindIcon kind={k} className="size-5" /><span className="truncate text-[12.5px]">{KIND_META[k].label}</span>
          </button>
        ))}
      </div>
      {conns.length > 0 && (
        <div className="mt-4 rounded-lg border border-ok/30 bg-ok/5 p-3 text-[13px]">
          <span className="text-ok">✓</span> {conns.length} connection{conns.length === 1 ? "" : "s"}: {conns.map((c) => c.name).join(", ")}
        </div>
      )}
      <p className="mt-3 text-[12px] text-faint">Behind a VPN or a jump host? Set those up in VPN & tunnels; a connection can use one and connects it first.</p>
    </>
  );
}

function AnywhereStep() {
  const cards: { icon: ReactNode; title: string; desc: string; open: () => void }[] = [
    { icon: <Smartphone className="size-5 text-accent" />, title: "Your phone", desc: "Follow and answer your agents with DevCat Mobile or the phone's browser.", open: () => useMobile.getState().show() },
    { icon: <Bot className="size-5 text-info" />, title: "Agent bots", desc: "Allow, ask and get results in Telegram or Discord.", open: openIntegrations },
    { icon: <Cable className="size-5 text-ok" />, title: "Sync MCP", desc: "Let Claude Code, Codex, Cursor and Gemini use your connections.", open: openMcpSync },
    { icon: <Brain className="size-5 text-warn" />, title: "AI memory", desc: "One memory every agent shares, across your computers.", open: () => openMemory() },
  ];
  return (
    <>
      <Title kicker="Step 4 · optional" title="Keep your agents going from anywhere">
        Leave an agent working and walk away. Set these up now or later: they're all in Settings and on the Welcome tab.
      </Title>
      <div className="grid grid-cols-2 gap-2">
        {cards.map((c) => (
          <button key={c.title} onClick={c.open} className="flex flex-col gap-2 rounded-lg border border-line bg-panel p-4 text-left hover:border-accent/60 hover:bg-raised">
            {c.icon}
            <div className="font-medium">{c.title}</div>
            <div className="text-[12px] text-muted">{c.desc}</div>
          </button>
        ))}
      </div>
    </>
  );
}

function ReadyStep() {
  const providers = useProviders((s) => s.providers);
  const selectedId = useProviders((s) => s.selectedId);
  const loaded = useProviders((s) => s.loaded);
  const cwd = useWorkspace((s) => s.cwd);
  const conns = useConnections((s) => s.connections.length);
  const agent = providers.find((p) => p.id === selectedId)?.name ?? "Claude Code";
  const rows: [string, string, boolean][] = [
    ["Coding agent", agent, true],
    ["Project folder", cwd ? cwd.split(/[\\/]/).pop()! : "Not yet: pick one in the AI workspace", !!cwd],
    ["Connections", conns ? `${conns}` : "None yet: add them from Connections", conns > 0],
  ];
  return (
    <div className="flex flex-col items-center pt-6 text-center">
      <div className="flex size-14 items-center justify-center rounded-2xl bg-ok/15 text-ok"><Check className="size-7" /></div>
      <h1 className="mt-5 text-3xl font-bold tracking-tight">You're set</h1>
      <div className="mt-6 w-full max-w-md rounded-lg border border-line bg-panel text-left">
        {!loaded ? <div className="p-4"><Spinner /></div> : rows.map(([k, v, ok]) => (
          <div key={k} className="flex items-center gap-3 border-b border-line px-4 py-2.5 text-[13px] last:border-0">
            <span className="w-32 text-muted">{k}</span><span className={clsx("flex-1", !ok && "text-faint")}>{v}</span>{ok && <Check className="size-4 text-ok" />}
          </div>
        ))}
      </div>
      <p className="mt-6 max-w-md text-[13px] text-muted">New here? The tour shows where everything is in about a minute.</p>
      <div className="mt-4 flex gap-2">
        <Button variant="primary" onClick={() => finishSetup(true)}>Take the tour</Button>
        {cwd && <Button onClick={() => { finishSetup(); openClaudeWorkspace(cwd); }}>Start a chat</Button>}
        <Button variant="ghost" onClick={() => finishSetup()}>Just look around</Button>
      </div>
    </div>
  );
}
