// Terminal (the second tab): every terminal on the computer, agents and plain shells, and new
// ones: a shell or Claude Code in a project folder. Opening one shows it live (agents.tsx Terminal).
import { CheckCircle2, ChevronRight, Laptop, Plus, Smartphone, Sparkles, TerminalSquare } from "lucide-react";
import { useEffect, useState } from "react";
import { host, nav, refreshAgents, toastErr, useApp } from "../lib/store";
import type { PtyInfo } from "../lib/types";
import { Empty, Screen, Section, Spinner, ago, duration, folderName } from "../ui";
import { termSize } from "./agents";
import { FolderSheet, cwdOf, useProjects } from "./chats";
import { ProjectBar } from "./focus";
import { focusFolders, useInFocus } from "../lib/focus";

export function Terminals() {
  const terms = useApp((s) => s.terms);
  const agents = useApp((s) => s.agents);
  const conn = useApp((s) => s.conn[s.current ?? ""]);
  const readonly = useApp((s) => s.machines.find((m) => m.id === s.current)?.access === "readonly");
  const projects = useProjects();
  const shown = useInFocus();
  const [pick, setPick] = useState<"shell" | "claude" | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    void refreshAgents();
    const t = setInterval(() => { if (!document.hidden) void refreshAgents(); }, 5000);
    return () => clearInterval(t);
  }, []);
  const online = conn?.state === "online";

  const open = async (cwd: string, claude: boolean) => {
    setBusy(true);
    try {
      const { cols, rows } = termSize();
      const info = await host<PtyInfo>("pty_open", { cwd, program: claude ? "claude" : null, args: [], cols, rows, providerId: null });
      useApp.setState({ terms: [...useApp.getState().terms.filter((t) => t.id !== info.id), info] });
      nav.push({ s: "terminal", id: info.id });
    } catch (e) {
      toastErr(e);
    } finally {
      setBusy(false);
    }
  };

  const shells = terms.filter((t) => !t.agent && shown(t.cwd));
  const agentTerms = terms.filter((t) => t.agent && shown(cwdOf(t, agents[t.id])));

  return (
    <Screen title="Terminal" sub="Runs on the computer" tab="terminals" className="space-y-3 p-3">
      <ProjectBar />
      {!readonly && (
        <div className="grid grid-cols-2 gap-2">
          <NewButton icon={<TerminalSquare className="size-5" />} title="New terminal" meta="A shell in a folder" disabled={!online || busy} onClick={() => setPick("shell")} />
          <NewButton icon={<Sparkles className="size-5 text-accent" />} title="New Claude Code" meta="In a terminal" disabled={!online || busy} onClick={() => setPick("claude")} />
        </div>
      )}
      {busy && <div className="flex items-center gap-2 text-[13px] text-muted"><Spinner />Opening…</div>}

      {shells.length > 0 && <Section>Shells</Section>}
      {shells.map((t) => (
        <TermRow key={t.id} t={t} title={folderName(t.cwd) || t.cwd} meta={`${t.program} · ${t.cwd}`} right={ago(t.startedAt)} />
      ))}

      {agentTerms.length > 0 && <Section>Agents</Section>}
      {agentTerms.map((t) => {
        const a = agents[t.id];
        const state = a?.status === "waiting" ? <span className="text-warn">needs you</span> : a?.status === "working" ? <span className="text-warn">working {duration(Date.now() - a.since)}</span> : a ? <span className="text-ok">done</span> : t.program;
        return <TermRow key={t.id} t={t} title={folderName(cwdOf(t, a))} meta={<>{t.program} · {state}</>} right={ago(t.startedAt)} done={a?.status === "idle"} />;
      })}

      {online && shells.length + agentTerms.length === 0 && (
        <Empty icon={<TerminalSquare className="size-8" />} title="No terminals open">Open a shell or Claude Code in one of your projects. It keeps running on the computer when you close this app.</Empty>
      )}
      {readonly && <div className="rounded-lg bg-hover px-3 py-2 text-center text-[12px] text-muted">Read-only access: you can watch terminals but not open or type in them.</div>}

      <FolderSheet open={pick !== null} value={focusFolders()[0] ?? projects.sorted[0]?.cwd ?? ""} title={pick === "claude" ? "Open Claude Code in" : "Open a terminal in"}
        onClose={() => setPick(null)} onPick={(cwd) => void open(cwd, pick === "claude")} />
    </Screen>
  );
}

function NewButton({ icon, title, meta, disabled, onClick }: { icon: React.ReactNode; title: string; meta: string; disabled: boolean; onClick: () => void }) {
  return (
    <button disabled={disabled} onClick={onClick} className="flex flex-col items-start gap-1.5 rounded-2xl border border-line bg-raised p-3 text-left active:bg-hover disabled:opacity-50">
      <span className="flex size-9 items-center justify-center rounded-full bg-hover">{icon}</span>
      <span className="flex items-center gap-1 font-semibold"><Plus className="size-3.5" />{title}</span>
      <span className="text-[11.5px] text-muted">{meta}</span>
    </button>
  );
}

function TermRow({ t, title, meta, right, done }: { t: PtyInfo; title: React.ReactNode; meta: React.ReactNode; right: string; done?: boolean }) {
  return (
    <button onClick={() => nav.push({ s: "terminal", id: t.id })} className="flex w-full items-center gap-3 rounded-xl bg-raised px-3 py-2.5 text-left active:bg-hover">
      <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-[#0a0b0d] font-mono text-[12px] text-ok">{done ? <CheckCircle2 className="size-4" /> : "›_"}</span>
      <span className="min-w-0 flex-1"><span className="block truncate font-semibold">{title}</span><span className="block truncate text-[12px] text-muted">{meta}</span></span>
      <span className="flex shrink-0 items-center gap-1 text-[11px] text-faint">{t.origin.startsWith("phone:") ? <Smartphone className="size-3" /> : <Laptop className="size-3" />}{right}</span>
      <ChevronRight className="size-4 shrink-0 text-faint" />
    </button>
  );
}
