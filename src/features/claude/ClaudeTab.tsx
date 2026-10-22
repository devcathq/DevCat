// AI workspace tab, like an agent CLI in iTerm2: the tab's AI connection (Claude Code by default, or
// Codex / Gemini / Copilot / Aider / opencode / custom) in a terminal (Chat), plus Diff and — for
// Claude — Code Review views, a git branch chip and a review badge in the header. Other agents get a
// "Review" button that types a review request into their terminal.
// ⌥⇧⌘1/2/3 switch views when the tab is active. "Terminal" (⌥⇧⌘T) opens a shell in the folder in
// its own tab.
import clsx from "clsx";
import {
  AlertOctagon, AlertTriangle, ArrowDown, ArrowUp, Bot, CheckCircle2, ChevronDown, GitBranch, MessageSquarePlus, RotateCcw, ScanSearch, SquareTerminal,
  Terminal,
  Sparkle,
} from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import { Button, Dialog, Empty, IconButton, Select, Spinner } from "../../components/ui";
import type { AiProvider, ClaudePermissionMode } from "../../lib/types";
import { useContextMenu } from "../connections/ContextMenu";
import { providerMenu } from "./AiProviders";
import { useTabs } from "../../store/tabs";
import type { Tab } from "../../tabs/types";
import { CopyButton, CodeBlock } from "./Markdown";
import { DiffView } from "./DiffView";
import type { ClaudeView } from "./open";
import {
  CLAUDE_PROVIDER_ID, detectProviders, installState, KINDS, loadProviders, REVIEW_PROMPT, useProvider, useProviders,
} from "./providers";
import { ReviewView, reviewBadge } from "./ReviewView";
import {
  claudeLaunchArgs, detectSession, ensureTab, loadInfo, newSession, pasteToClaude, refreshGit, restartClaude, sendToAgent, setLaunchOptions, setPty,
  setTabIsClaude, startReview, useClaude, type Finding,
} from "./store";
import { setTabVisible, useTabActivity } from "./activity";
import { TerminalPane } from "./TerminalPane";
import { basename, modelOptions, PERMISSION_MODES } from "./util";
import { isRemote } from "../../lib/remotePath";
import AgentChat from "../agent/AgentChat";
import { closeChat, send as sendToChat, useAgent } from "../agent/store";
import { openTerminal } from "../terminal/open";

type ClaudeTabT = Extract<Tab, { type: "claude" }>;

const IS_MAC = navigator.platform.toLowerCase().includes("mac");

const VIEWS: Array<{ id: ClaudeView; label: string; key: string }> = [
  { id: "chat", label: "Chat", key: "1" },
  { id: "diff", label: "Diff", key: "2" },
  { id: "review", label: "Code Review", key: "3" },
];

function BranchChip({ tabId, onClick }: { tabId: string; onClick: () => void }) {
  const git = useClaude((s) => s.tabs[tabId]?.git);
  if (!git) return null;
  if (!git.isRepo) return <span className="text-[11px] text-faint">no git</span>;
  const n = git.files.length;
  const plural = (k: number, w: string) => `${k} ${w}${k === 1 ? "" : "s"}`;
  const counts = [git.ahead && `${plural(git.ahead, "commit")} ahead`, git.behind && `${plural(git.behind, "commit")} behind`, n && plural(n, "changed file")].filter(Boolean);
  return (
    <button type="button" onClick={onClick} title={[git.branch ?? "detached HEAD", git.root, counts.join(", ")].filter(Boolean).join("\n")}
      className="inline-flex h-6 items-center gap-1.5 rounded-md border border-line bg-raised px-2 text-[11px] whitespace-nowrap text-muted hover:bg-hover">
      <GitBranch className="size-3 shrink-0" />
      <span className="max-w-40 truncate text-fg/90 @max-5xl:max-w-24">{git.branch ?? "detached"}</span>
      {git.ahead > 0 && <span className="inline-flex shrink-0 items-center text-info"><ArrowUp className="size-3" />{git.ahead}</span>}
      {git.behind > 0 && <span className="inline-flex shrink-0 items-center text-warn"><ArrowDown className="size-3" />{git.behind}</span>}
      {n > 0 && <span className="inline-flex shrink-0 items-center gap-0.5 text-warn"><span className="size-1.5 rounded-full bg-current" />{n}</span>}
    </button>
  );
}

function ReviewBadge({ tabId, onClick }: { tabId: string; onClick: () => void }) {
  const r = useClaude((s) => s.tabs[tabId]?.review);
  const b = reviewBadge(r);
  if (b.kind === "none") return null;
  return (
    <button type="button" onClick={onClick} title="Code review"
      className={clsx("inline-flex h-6 items-center gap-1 rounded-md border px-2 text-[11px]",
        b.kind === "running" && "border-line text-muted",
        b.kind === "ok" && "border-ok/40 text-ok",
        b.kind === "error" && "border-danger/40 text-danger",
        b.kind === "issues" && (b.worst === "critical" ? "border-danger/40 text-danger" : "border-warn/40 text-warn"))}>
      {b.kind === "running" && <><Spinner className="size-3" />Reviewing</>}
      {b.kind === "ok" && <><CheckCircle2 className="size-3" />No issues</>}
      {b.kind === "error" && <><AlertOctagon className="size-3" />Review failed</>}
      {b.kind === "issues" && <><AlertTriangle className="size-3" />{b.count ? `${b.count} finding${b.count === 1 ? "" : "s"}` : "Review"}</>}
    </button>
  );
}

function InstallHelp({ onRetry, error }: { onRetry: () => void; error?: string | null }) {
  return (
    <Empty icon={<Terminal className="size-8" />} title="Claude Code is not installed">
      <div className="flex flex-col gap-2 text-left">
        <p>DevCat runs your local <code className="text-accent">claude</code> CLI. Install it, then run <code className="text-accent">claude</code> once in a terminal to log in.</p>
        <div className="text-[11px] text-muted">macOS / Linux</div>
        <CodeBlock code="curl -fsSL https://claude.ai/install.sh | bash" lang="sh" />
        <div className="text-[11px] text-muted">Windows (PowerShell)</div>
        <CodeBlock code="irm https://claude.ai/install.ps1 | iex" lang="powershell" />
        {error && <p className="text-[11px] text-danger">{error}</p>}
        <Button className="self-center" onClick={onRetry}>Check again</Button>
      </div>
    </Empty>
  );
}

/** Not-installed state for agents other than Claude (install hint only — nothing is run). */
function AgentInstallHelp({ provider }: { provider: AiProvider }) {
  const kind = KINDS[provider.kind];
  const checking = useProviders((s) => s.detecting);
  return (
    <Empty icon={<Terminal className="size-8" />} title={`${kind.label} is not installed`}>
      <div className="flex flex-col gap-2 text-left">
        <p>DevCat runs your local <code className="text-accent">{kind.program}</code> CLI. Install it, then check again.</p>
        {kind.install && <CodeBlock code={kind.install} lang="sh" />}
        <p className="text-[11px] text-faint">{kind.login}</p>
        <Button className="self-center" loading={checking} onClick={() => void detectProviders(true)}>Check again</Button>
      </div>
    </Empty>
  );
}

/** The tab's AI connection; switching restarts the terminal with the new agent. */
function AgentChip({ tabId, cwd, provider }: { tabId: string; cwd: string; provider: AiProvider }) {
  const menu = useContextMenu();
  const st = useProviders((s) => installState(provider, s.detect));
  const pick = (p: AiProvider) => {
    if (p.id === provider.id) return;
    const isClaude = p.id === CLAUDE_PROVIDER_ID;
    useTabs.getState().update(tabId, {
      providerId: isClaude ? null : p.id,
      sessionId: null,
      title: isClaude ? basename(cwd) : `${basename(cwd)} · ${p.name}`,
    } as never);
  };
  return (
    <>
      <button type="button"
        onClick={(e) => {
          const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
          menu.show({ clientX: r.left, clientY: r.bottom + 2 }, providerMenu(provider.id, pick));
        }}
        title={`AI connection of this tab: ${provider.name}${st === "missing" ? " (not installed)" : ""} — switching restarts the chat`}
        className="inline-flex h-6 max-w-40 shrink-0 items-center gap-1.5 rounded-md border border-line bg-raised px-2 text-[11px] whitespace-nowrap text-muted hover:bg-hover">
        <Bot className="size-3 shrink-0" />
        <span className="truncate text-fg/90 @max-5xl:hidden">{provider.name}</span>
        <ChevronDown className="size-3 shrink-0" />
      </button>
      {menu.element}
    </>
  );
}

/** Model / permission mode the claude process is launched with; offers a restart when changed. */
function LaunchOptions({ tabId, onBypass }: { tabId: string; onBypass: () => void }) {
  const info = useClaude((s) => s.info);
  const options = useClaude((s) => s.tabs[tabId]?.options);
  const launched = useClaude((s) => s.tabs[tabId]?.launched);
  const models = useMemo(() => modelOptions(info?.models), [info]);
  if (!options) return null;
  const changed = !!launched && (launched.model !== options.model || launched.permissionMode !== options.permissionMode);
  return (
    <>
      <Select className="h-6 w-24 shrink-0 text-xs" value={options.model} title="Model claude starts with (use /model to switch mid-session)"
        onChange={(e) => setLaunchOptions(tabId, { model: e.target.value })}>
        {models.map((m) => <option key={m.value} value={m.value}>{m.label}</option>)}
      </Select>
      <Select
        className={clsx("h-6 w-36 shrink-0 text-xs", options.permissionMode === "bypassPermissions" && "border-danger/60 text-danger")}
        value={options.permissionMode}
        title="Permission mode claude starts with (⇧Tab cycles modes in the TUI)"
        onChange={(e) => {
          const m = e.target.value as ClaudePermissionMode | "";
          if (m === "bypassPermissions") onBypass();
          else setLaunchOptions(tabId, { permissionMode: m });
        }}
      >
        <option value="">Default mode</option>
        {PERMISSION_MODES.filter((m) => m.value !== "manual").map((m) => <option key={m.value} value={m.value}>{m.label}</option>)}
      </Select>
      {changed && (
        <Button size="sm" variant="primary" className="shrink-0" onClick={() => void restartClaude(tabId)} title="Restart claude with these options, resuming the session">
          <RotateCcw className="size-3" /><span className="@max-6xl:hidden">Restart claude</span>
        </Button>
      )}
    </>
  );
}

function SessionChip({ tabId }: { tabId: string }) {
  const sid = useClaude((s) => s.tabs[tabId]?.sessionId);
  if (!sid) return null;
  return (
    <span className="inline-flex shrink-0 items-center text-[10px] whitespace-nowrap text-faint @max-[104rem]:hidden" title={`Session ${sid}`}>
      <span className="font-mono">{sid.slice(0, 8)}</span><CopyButton text={sid} />
    </span>
  );
}

function ChatPane({ tabId, cwd, visible, provider }: { tabId: string; cwd: string; visible: boolean; provider: AiProvider }) {
  const isClaude = provider.kind === "claude";
  const startSeq = useClaude((s) => s.tabs[tabId]?.startSeq ?? 0);
  return (
    <TerminalPane
      cwd={cwd}
      program="claude"
      providerId={provider.id}
      name={isClaude ? "claude" : provider.name}
      startKey={startSeq}
      visible={visible}
      shiftEnterNewline={isClaude}
      activityId={tabId}
      getArgs={() => claudeLaunchArgs(tabId)}
      beforeRestart={async () => { await detectSession(tabId); }}
      onPty={(id) => setPty(tabId, id)}
    />
  );
}


export default function ClaudeTab({ tab }: { tab: ClaudeTabT }) {
  const tabId = tab.id;
  const ready = useClaude((s) => !!s.tabs[tabId]);
  useLayoutEffect(() => ensureTab(tabId, tab.cwd, tab.sessionId), [tabId, tab.cwd, tab.sessionId]);
  const provider = useProvider(tab.providerId);
  const isClaude = provider?.kind === "claude";
  // DevCat Agent: a chat view instead of a terminal
  const isAgent = provider?.kind === "agent";
  const agentBusy = useAgent((s) => !!s.chats[tabId]?.busy);
  const agent = isClaude ? "Claude" : provider?.name ?? "the agent";
  useLayoutEffect(() => { if (ready && provider) setTabIsClaude(tabId, isClaude); }, [ready, provider, isClaude, tabId]);
  const active = useTabs((s) => s.activeId === tabId);
  const activity = useTabActivity(tabId);
  // "Seen" = the tab is shown in a focused window; clears the finished marker / Dock badge.
  useEffect(() => {
    setTabVisible(tabId, active);
    return () => setTabVisible(tabId, false);
  }, [tabId, active]);
  // Diff and Code Review use git on this computer: not for folders on SSH hosts (yet).
  const remote = isRemote(tab.cwd);
  const views = useMemo(() => VIEWS.filter((v) => !(remote && (v.id === "diff" || v.id === "review")) && (isClaude || v.id !== "review")), [isClaude, remote]);
  const wanted: ClaudeView = tab.view ?? "chat";
  const view: ClaudeView = views.some((v) => v.id === wanted) ? wanted : "chat";
  const info = useClaude((s) => s.info);
  const infoLoaded = useClaude((s) => s.infoLoaded);
  const infoError = useClaude((s) => s.infoError);
  const missing = useProviders((s) => (provider ? installState(provider, s.detect) === "missing" : false));
  const running = useClaude((s) => !!s.tabs[tabId]?.ptyId);
  const [confirmBypass, setConfirmBypass] = useState(false);

  useEffect(() => {
    void loadProviders();
    void detectProviders();
  }, []);
  // Claude's account / models (for the launch options) — only Claude tabs need them.
  useEffect(() => { if (isClaude) void loadInfo(); }, [isClaude]);

  const setView = useCallback((v: ClaudeView) => useTabs.getState().update(tabId, { view: v } as never), [tabId]);

  // Git status: poll every 5s while this tab is visible.
  useEffect(() => {
    if (!active) return;
    void refreshGit(tabId);
    const t = setInterval(() => void refreshGit(tabId), 5000);
    return () => clearInterval(t);
  }, [active, tabId]);

  // ⌥⇧⌘1/2/3 (Ctrl+Alt+Shift elsewhere) — only for the active tab.
  useEffect(() => {
    if (!active) return;
    const h = (e: KeyboardEvent) => {
      if (!e.altKey || !e.shiftKey || !(e.metaKey || e.ctrlKey)) return;
      if (e.code === "KeyT") {
        e.preventDefault();
        openTerminal(tab.cwd);
        return;
      }
      const v = views.find((x) => e.code === `Digit${x.key}`);
      if (!v) return;
      e.preventDefault();
      setView(v.id);
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, [active, setView, views, tab.cwd]);

  // Claude: structured review run (Code Review view). Others: ask the agent in its terminal.
  const reviewNow = () => {
    if (isAgent) {
      setView("chat");
      void sendToChat(tabId, REVIEW_PROMPT);
      return;
    }
    if (isClaude) {
      setView("review");
      void startReview(tabId);
      return;
    }
    setView("chat");
    void sendToAgent(tabId, REVIEW_PROMPT).then((ok) => {
      if (!ok) toast.error(`${agent} isn't running in this tab — press Enter in the terminal to start it.`);
    });
  };

  // Paste the finding into claude's prompt (not submitted) so the user can edit it first.
  const fixFinding = async (f: Finding) => {
    const loc = f.file ? `${f.file}${f.line ? `:${f.line}` : ""}` : "";
    const parts = [
      `Fix this review finding${loc ? ` in ${loc}` : ""}: [${f.severity}] ${f.title}`,
      f.body ?? "",
      f.suggestion ? `Suggested fix:\n${f.suggestion}` : "",
    ].filter(Boolean);
    setView("chat");
    if (!(await pasteToClaude(tabId, parts.join("\n\n")))) toast.error("Claude isn't running in this tab — press Enter in the terminal to start it.");
  };

  if (!ready || provider === undefined) return null;
  // Claude Code with its default binary: claude_info knows whether it's installed.
  const notInstalled = isAgent ? false : isClaude && !provider?.command ? infoLoaded && info !== null && !info.path : missing;

  return (
    <div className="flex h-full min-h-0 flex-col bg-app">
      {/* As the header narrows, its parts give way in this order: the shortcut hints, the button
          labels (icons stay, with tooltips), the folder path and session id, the branch name gets
          shorter, then the folder name and connection name go (the tab title has them). Past that, whole items
          move to a second line; a chip never breaks. */}
      <div className="@container shrink-0 border-b border-line bg-panel">
      <div className="flex min-h-10 min-w-0 items-center gap-x-2 gap-y-1 overflow-hidden px-3 py-1.5 @max-[56rem]:flex-wrap">
        <div className="flex max-w-56 min-w-12 shrink flex-col leading-tight @max-5xl:hidden" title={tab.cwd}>
          <span className="truncate text-[13px] font-semibold">{basename(tab.cwd)}</span>
          <span className="truncate text-[10px] text-faint @max-6xl:hidden">{tab.cwd}</span>
        </div>
        <div className="ml-1 flex h-7 shrink-0 items-center rounded-md border border-line bg-app p-0.5">
          {views.map((v) => (
            <button
              key={v.id}
              type="button"
              title={`${v.label} (${IS_MAC ? "⌥⇧⌘" : "Ctrl+Alt+Shift+"}${v.key})`}
              onClick={() => setView(v.id)}
              className={clsx("flex h-6 items-center gap-1.5 rounded px-2.5 text-xs whitespace-nowrap", view === v.id ? "bg-raised text-fg shadow-sm" : "text-muted hover:text-fg")}
            >
              {v.id === "chat" && activity.working && <Sparkle className="size-3 animate-spin text-accent [animation-duration:2.4s]" />}
              {v.label}
              <span className="text-[9px] text-faint @max-[104rem]:hidden">{IS_MAC ? `⌥⇧⌘${v.key}` : `^⌥⇧${v.key}`}</span>
            </button>
          ))}
        </div>
        <Button size="sm" className="shrink-0" title={`Open a terminal in this folder, in its own tab (${IS_MAC ? "⌥⇧⌘T" : "Ctrl+Alt+Shift+T"})`} onClick={() => openTerminal(tab.cwd)}>
          <SquareTerminal className="size-3.5" /><span className="@max-7xl:hidden">Terminal</span>
        </Button>
        {provider && <AgentChip tabId={tabId} cwd={tab.cwd} provider={provider} />}
        <BranchChip tabId={tabId} onClick={() => setView("diff")} />
        {isClaude && <ReviewBadge tabId={tabId} onClick={() => setView("review")} />}
        <div className="flex-1" />
        {(isAgent ? agentBusy : activity.working) && <span className="shimmer-text shrink-0 text-[11px] font-medium whitespace-nowrap" title={`${agent} is working`}>working</span>}
        {!isAgent && <span className={clsx("size-1.5 shrink-0 rounded-full", activity.working ? "bg-accent" : running ? "bg-ok" : "bg-faint")} title={activity.working ? `${agent} is working` : running ? `${agent} is running` : `${agent} is not running`} />}
        {isClaude && <SessionChip tabId={tabId} />}
        {isClaude && <LaunchOptions tabId={tabId} onBypass={() => setConfirmBypass(true)} />}
        {provider && !isClaude && (
          <Button size="sm" variant="ghost" className="shrink-0" title={isAgent ? `Ask ${agent} to review the uncommitted changes` : `Ask ${agent} to review the uncommitted changes (typed into its terminal)`} onClick={reviewNow}>
            <ScanSearch className="size-3.5" /><span className="@max-7xl:hidden">Review</span>
          </Button>
        )}
        <Button size="sm" variant="ghost" className="shrink-0" disabled={!provider} title={`Start a new ${agent} session`} onClick={() => {
          setView("chat");
          if (isAgent) void closeChat(tabId).then(() => useTabs.getState().update(tabId, { sessionId: null } as never));
          else newSession(tabId);
        }}>
          <MessageSquarePlus className="size-3.5" /><span className="@max-7xl:hidden">New session</span>
        </Button>
        {view === "diff" && provider && (
          <IconButton title={`Ask ${agent} to review`} onClick={reviewNow}><ScanSearch className="size-3.5" /></IconButton>
        )}
      </div>
      </div>
      <div className="relative min-h-0 flex-1">
        <div className={clsx("absolute inset-0", view !== "chat" && "invisible")}>
          {!provider ? (
            <Empty icon={<Bot className="size-8" />} title="AI connection not found">
              <div className="flex flex-col items-center gap-2">
                <p>The AI connection this chat used was deleted.</p>
                <Button onClick={() => useTabs.getState().update(tabId, { providerId: null, title: basename(tab.cwd) } as never)}>Use Claude Code</Button>
              </div>
            </Empty>
          ) : notInstalled ? (
            isClaude ? <InstallHelp onRetry={() => void loadInfo(true)} error={infoError} /> : <AgentInstallHelp provider={provider} />
          ) : (
            isAgent ? (
              <AgentChat tabId={tabId} cwd={tab.cwd} provider={provider} visible={active && view === "chat"} onReview={() => setView("diff")} />
            ) : (
              <ChatPane tabId={tabId} cwd={tab.cwd} visible={active && view === "chat"} provider={provider} />
            )
          )}
        </div>
        {view === "diff" && <div className="absolute inset-0 bg-app"><DiffView tabId={tabId} cwd={tab.cwd} active={active} onReview={reviewNow} agent={agent} /></div>}
        {isClaude && (
          <div className={clsx("absolute inset-0 bg-app", view !== "review" && "hidden")}>
            <ReviewView tabId={tabId} cwd={tab.cwd} onFix={(f) => void fixFinding(f)} onShowDiff={() => setView("diff")} />
          </div>
        )}
      </div>
      <Dialog
        open={confirmBypass}
        onClose={() => setConfirmBypass(false)}
        title={<span className="flex items-center gap-2 text-danger"><AlertTriangle className="size-4" />Bypass all permissions?</span>}
        width={460}
        footer={
          <>
            <Button variant="ghost" onClick={() => setConfirmBypass(false)}>Cancel</Button>
            <Button variant="danger" onClick={() => { setConfirmBypass(false); setLaunchOptions(tabId, { permissionMode: "bypassPermissions" }); }}>Bypass permissions</Button>
          </>
        }
      >
        <div className="flex flex-col gap-2 text-[13px] text-fg/90">
          <p>Claude will run <b>every</b> tool — shell commands, file edits and deletions, network access — without asking.</p>
          <p className="text-danger">Only use this in a sandbox or a throwaway checkout of {basename(tab.cwd)}.</p>
        </div>
      </Dialog>
    </div>
  );
}
