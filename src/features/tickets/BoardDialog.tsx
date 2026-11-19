// Connect a board (Jira project, Trello board, Linear team, GitHub repository) and say what the agent does with it:
// which column it takes tickets from, where they go while it works, when fixed and when it needs a
// person; the project, repository and AI connection it works with; and an optional Slack webhook.
import { open } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import clsx from "clsx";
import { ExternalLink, FolderOpen, RefreshCw } from "lucide-react";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { Button, Checkbox, Dialog, ErrorBox, Field, Input, Select, Textarea } from "../../components/ui";
import { api, errMsg } from "../../lib/api";
import type { AiProvider } from "../../lib/types";
import { useConnections } from "../../store/connections";
import { ASSIGNED, TRACKERS, newBoard, tickets, type Board, type Column, type Isolation, type TrackerKind } from "./api";
import { projectForFolder } from "./store";

const TOKEN_HELP: Record<TrackerKind, { url: string; text: string }> = {
  jira: { url: "https://id.atlassian.com/manage-profile/security/api-tokens", text: "Create an API token" },
  trello: { url: "https://trello.com/power-ups/admin", text: "Get an API key and token" },
  linear: { url: "https://linear.app/settings/account/security", text: "Create a personal API key" },
  github: { url: "https://github.com/settings/personal-access-tokens/new", text: "Create a token" },
};

export function BoardDialog({ board, onClose, onSaved }: { board: Board | null; onClose: () => void; onSaved: (b: Board) => void }) {
  const [b, setB] = useState<Board>(board ?? newBoard());
  const [token, setToken] = useState("");
  const [webhook, setWebhook] = useState<string | undefined>(undefined);
  const [columns, setColumns] = useState<Column[]>([]);
  const [providers, setProviders] = useState<AiProvider[]>([]);
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const isNew = !board;
  // Commit to an existing branch (main, staging…) instead of a new one per ticket.
  const [existing, setExisting] = useState(!!board?.targetBranch);
  // A worktree for each ticket: always a new branch, made from the base branch (`targetBranch`).
  const wt = b.isolation === "worktree";
  // GitHub: labels are the columns, a new `<login>/…` branch per issue, always pushed (for its pull request).
  const gh = b.kind === "github";
  const onto = existing && !wt && !gh;
  const set = (p: Partial<Board>) => setB((x) => ({ ...x, ...p }));
  const projects = useConnections((s) => s.projects);
  const detected = projectForFolder(projects, b.repo);
  // Picking a project with code folders fills in the repository when it's empty.
  const pickProject = (id: string) => {
    const p = projects.find((x) => x.id === id);
    set({ projectId: id || null, ...(p && !b.repo.trim() && p.folders?.length ? { repo: p.folders[0] } : {}) });
  };

  useEffect(() => {
    void api.aiProvidersList().then((l) => setProviders(l.filter((p) => p.kind === "claude" || p.kind === "agent"))).catch(() => {});
    if (board?.hasToken) void loadColumns(board);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const withSecrets = (x: Board): Board => ({ ...x, token: token.trim() || undefined, webhook });

  const loadColumns = async (x: Board = b) => {
    setLoading(true);
    setError(null);
    try {
      const cols = await tickets.columns(withSecrets(x));
      setColumns(cols);
      // Sensible first picks on a new board.
      if (!x.pickFrom && cols.length) {
        const find = (...names: string[]) => cols.find((c) => names.some((n) => c.name.toLowerCase() === n)) ?? null;
        set({
          pickFrom: find("to do", "todo", "backlog", "open") ?? cols[0],
          working: find("in progress", "doing"),
          doneTo: find("testing", "in testing", "qa", "in review", "review", "done"),
        });
      }
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setLoading(false);
    }
  };

  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      if (onto && !b.targetBranch.trim()) throw new Error("Type the branch the agent commits to (main, staging…)");
      const saved = await tickets.save(withSecrets({ ...b, targetBranch: onto || wt ? b.targetBranch : "" }));
      toast.success(isNew ? "Board connected" : "Board saved");
      onSaved(saved);
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setSaving(false);
    }
  };

  const pickRepo = async () => {
    const dir = await open({ directory: true, multiple: false, defaultPath: b.repo || undefined });
    if (typeof dir === "string") set({ repo: dir });
  };

  const colSelect = (value: Column | null | undefined, onChange: (c: Column | null) => void, none?: string) => (
    <Select value={value?.id ?? ""} onChange={(e) => onChange(columns.find((c) => c.id === e.target.value) ?? null)} disabled={!columns.length && !value}>
      {none !== undefined && <option value="">{none}</option>}
      {none === undefined && !value && <option value="">Choose…</option>}
      {value && !columns.some((c) => c.id === value.id) && <option value={value.id}>{value.name}</option>}
      {columns.map((c) => <option key={c.id} value={c.id}>{c.name}</option>)}
    </Select>
  );

  const help = TOKEN_HELP[b.kind];
  const canSave = b.project.trim() && (token.trim() || b.hasToken);
  return (
    <Dialog
      open
      onClose={onClose}
      title={isNew ? "Connect a board" : `Board settings: ${board.name}`}
      width={640}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" onClick={() => void save()} loading={saving} disabled={!canSave}>{isNew ? "Connect" : "Save"}</Button>
        </>
      }
    >
      <div className="space-y-4">
        <section className="space-y-3">
          <div className="flex gap-1">
            {(Object.keys(TRACKERS) as TrackerKind[]).map((k) => (
              <button
                key={k}
                disabled={!isNew}
                onClick={() => {
                  const { site, isolation, push, branchPrefix, pickFrom, working, doneTo, failedTo } = newBoard(k);
                  set({ kind: k, site, isolation, push, branchPrefix, pickFrom, working, doneTo, failedTo });
                  setColumns([]);
                }}
                className={clsx("h-7 rounded-md border px-3 text-[13px]", b.kind === k ? "border-accent bg-accent/15 text-accent" : "border-line bg-raised text-muted hover:bg-hover", !isNew && b.kind !== k && "opacity-40")}
              >
                {TRACKERS[k]}
              </button>
            ))}
          </div>
          {b.kind === "jira" && (
            <div className="grid grid-cols-2 gap-3">
              <Field label="Site" className="col-span-2"><Input value={b.site} onChange={(e) => set({ site: e.target.value })} placeholder="https://acme.atlassian.net" /></Field>
              <Field label="Email" hint="Jira Cloud. Leave empty for a Data Center personal access token."><Input value={b.user} onChange={(e) => set({ user: e.target.value })} placeholder="you@acme.com" /></Field>
              <Field label="Project key"><Input value={b.project} onChange={(e) => set({ project: e.target.value.toUpperCase() })} placeholder="ABC" /></Field>
            </div>
          )}
          {b.kind === "trello" && (
            <div className="grid grid-cols-2 gap-3">
              <Field label="API key"><Input value={b.user} onChange={(e) => set({ user: e.target.value })} /></Field>
              <Field label="Board" hint="Its link (trello.com/b/…) or id."><Input value={b.project} onChange={(e) => set({ project: e.target.value })} placeholder="https://trello.com/b/AbC123/my-board" /></Field>
            </div>
          )}
          {b.kind === "linear" && (
            <Field label="Team key" hint="The prefix of the team's issues: ENG for ENG-123."><Input value={b.project} onChange={(e) => set({ project: e.target.value.toUpperCase() })} placeholder="ENG" /></Field>
          )}
          {gh && (
            <div className="grid grid-cols-2 gap-3">
              <Field label="Repository" hint="owner/name"><Input value={b.project} onChange={(e) => set({ project: e.target.value.trim() })} placeholder="acme/shop" /></Field>
              <Field label="API address" hint="GitHub Enterprise Server: https://<host>/api/v3"><Input value={b.site} onChange={(e) => set({ site: e.target.value })} placeholder="https://api.github.com" /></Field>
            </div>
          )}
          <Field
            label={b.kind === "linear" ? "API key" : b.kind === "trello" ? "Token" : gh ? "Personal access token" : "API token"}
            hint={gh ? "Fine-grained (Issues, Pull requests and Contents: read and write) or classic (repo scope). Labels, comments and pull requests appear as its owner. Kept in the system keychain." : "Kept in the system keychain."}
          >
            <div className="flex gap-2">
              <Input type="password" value={token} onChange={(e) => setToken(e.target.value)} placeholder={b.hasToken ? "Saved (type to replace)" : ""} />
              <Button onClick={() => void openUrl(help.url)} title={help.url}><ExternalLink className="size-3.5" />{help.text}</Button>
            </div>
          </Field>
        </section>

        <section className="space-y-3 border-t border-line pt-4">
          <div className="flex items-center justify-between">
            <div className="text-xs font-semibold tracking-wide text-muted uppercase">Workflow</div>
            <Button size="sm" onClick={() => void loadColumns()} loading={loading} disabled={!b.project.trim() || !(token.trim() || b.hasToken)}>
              <RefreshCw className="size-3" />{columns.length ? "Reload columns" : "Load columns"}
            </Button>
          </div>
          <div className="grid grid-cols-2 gap-3">
            <Field label={gh ? "Take issues labelled" : "Take tickets from"}>{colSelect(b.pickFrom, (c) => set({ pickFrom: c }))}</Field>
            <Field label={gh ? "While the agent works, label" : "While the agent works, move to"}>{colSelect(b.working, (c) => set({ working: c }), "Leave it where it is")}</Field>
            <Field label={gh ? "When fixed (draft pull request), label" : "When fixed, move to"}>{colSelect(b.doneTo, (c) => set({ doneTo: c }))}</Field>
            <Field label={gh ? "When it needs a person, label" : "When it needs a person, move to"}>{colSelect(b.failedTo, (c) => set({ failedTo: c }), "Back where it was")}</Field>
            <Field label="Assigned to" hint="Me: whoever the token belongs to.">
              <Select value={b.assigned} onChange={(e) => set({ assigned: e.target.value as Board["assigned"] })}>
                {(Object.keys(ASSIGNED) as Board["assigned"][]).map((k) => <option key={k} value={k}>{ASSIGNED[k]}</option>)}
              </Select>
            </Field>
            {!gh && <Field label="Only tickets labelled" hint="Empty: every ticket in the column."><Input value={b.label} onChange={(e) => set({ label: e.target.value })} placeholder="agent" /></Field>}
            <Field label="Board name"><Input value={b.name} onChange={(e) => set({ name: e.target.value })} placeholder={`${TRACKERS[b.kind]} ${b.project}`.trim()} /></Field>
          </div>
        </section>

        <section className="space-y-3 border-t border-line pt-4">
          <div className="text-xs font-semibold tracking-wide text-muted uppercase">Agent</div>
          <Field label="Project" hint="The agent gets this project's shared memory and connections, the same ones a chat in it uses.">
            <Select value={b.projectId ?? ""} onChange={(e) => pickProject(e.target.value)}>
              <option value="">{detected ? `From the repository: ${detected.name}` : "From the repository (no project holds it: its own memory)"}</option>
              {b.projectId && !projects.some((p) => p.id === b.projectId) && <option value={b.projectId}>(deleted project)</option>}
              {projects.map((p) => <option key={p.id} value={p.id}>{p.name}{p.folders?.length ? "" : " (no code folders)"}</option>)}
            </Select>
          </Field>
          <Field label="Repository" hint="A git repository, or a folder of them (the agent changes the ones the ticket needs). Each gets the ticket's branch (below); a repository with uncommitted changes is left alone.">
            <div className="flex gap-2">
              <Input value={b.repo} onChange={(e) => set({ repo: e.target.value })} placeholder="/path/to/repo or /path/to/folder-of-repos" />
              <Button onClick={() => void pickRepo()}><FolderOpen className="size-3.5" />Choose</Button>
            </div>
          </Field>
          <Field label="Working copy" hint={wt ? "Each ticket gets a git worktree in the repository's .worktrees folder, made from origin and removed after the run (its branch stays). You can keep working in the repository meanwhile. One repository, not a folder." : "The agent works in the repository itself, which must have no uncommitted changes."}>
            <Select value={b.isolation} onChange={(e) => set({ isolation: e.target.value as Isolation })}>
              <option value="inPlace">In place</option>
              <option value="worktree">A worktree for each ticket</option>
            </Select>
          </Field>
          {wt && (
            <div className="grid grid-cols-2 gap-3">
              <Field label="Files to copy" hint="One path a line, from the repository into each worktree: files git ignores, e.g. .env (a worktree has only the committed files).">
                <Textarea rows={2} value={b.copyFiles.join("\n")} onChange={(e) => set({ copyFiles: e.target.value.split("\n") })} placeholder={".env\nconfig/local.yml"} />
              </Field>
              <Field label="Setup command" hint="Runs in each new worktree before the agent starts. Needs “Let the agent run commands”. If it fails, the run fails with its output.">
                <Textarea rows={2} value={b.setupCommand} onChange={(e) => set({ setupCommand: e.target.value })} placeholder="pnpm install" />
              </Field>
            </div>
          )}
          <div className="grid grid-cols-2 gap-3">
            {wt ? (
              <Field label="Base branch" hint="New branches start from it on origin. Empty: origin's default branch.">
                <Input value={b.targetBranch} onChange={(e) => set({ targetBranch: e.target.value })} placeholder="main" />
              </Field>
            ) : gh ? null : <Field label="Branch" hint={existing ? "Fixed tickets are committed straight onto it (it must exist). Unfinished work goes on a new branch instead." : "Each ticket gets its own branch, made from the one the repository is on."}>
              <Select value={existing ? "existing" : "new"} onChange={(e) => setExisting(e.target.value === "existing")}>
                <option value="new">A new branch per ticket</option>
                <option value="existing">An existing branch</option>
              </Select>
            </Field>}
            {gh ? (
              <Field label="Branches" hint="Named after the token's owner (read when you save), e.g. for issue 12.">
                <Input value={`${b.login || "<login>"}/12-fix-the-login`} disabled />
              </Field>
            ) : onto ? (
              <Field label="Branch name" hint="E.g. main or staging.">
                <Input value={b.targetBranch} onChange={(e) => set({ targetBranch: e.target.value })} placeholder="main" />
              </Field>
            ) : (
              <Field label="Branch prefix" hint={`Branches look like ${b.branchPrefix.trim()}abc-12-fix-the-login.`}>
                <Input value={b.branchPrefix} onChange={(e) => set({ branchPrefix: e.target.value })} placeholder="devcat/" />
              </Field>
            )}
          </div>
          {wt && (
            <Field label="Parallel runs" hint="Tickets the agent works on at the same time (1 to 4), each in its own worktree and branch. The others wait.">
              <Input type="number" min={1} max={4} className="w-16" value={b.parallelRuns} onChange={(e) => set({ parallelRuns: Number(e.target.value) || 1 })} />
            </Field>
          )}
          <div className="grid grid-cols-2 gap-3">
            <Field label="AI connection" hint="Claude Code or a DevCat Agent connection.">
              <Select value={b.providerId} onChange={(e) => set({ providerId: e.target.value })}>
                {!providers.some((p) => p.id === b.providerId) && <option value={b.providerId}>{b.providerId === "claude" ? "Claude Code" : "(missing connection)"}</option>}
                {providers.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
              </Select>
            </Field>
            <Field label="Model" hint="Empty: the connection's default."><Input value={b.model ?? ""} onChange={(e) => set({ model: e.target.value || null })} placeholder="sonnet" /></Field>
          </div>
          <Field label="Default prompt" hint="Given to the agent with every ticket: conventions, where things are, how to test. When you fix tickets by hand, “With a prompt…” replaces it for those tickets.">
            <Textarea rows={4} value={b.instructions} onChange={(e) => set({ instructions: e.target.value })} placeholder="Run `pnpm test` before finishing. Keep the API backwards compatible." />
          </Field>
          <div className="flex flex-col gap-2">
            <Checkbox label="Let the agent run commands (builds, tests) without asking" checked={b.allowCommands} onChange={(v) => set({ allowCommands: v })} />
            <Checkbox label="Comment on the ticket with what was done and how to test it" checked={b.comment} onChange={(v) => set({ comment: v })} />
            {gh
              ? <p className="text-[13px] text-muted">A fixed issue's branch is pushed and gets a draft pull request into {wt && b.targetBranch.trim() ? b.targetBranch.trim() : "the default branch"} that closes the issue.</p>
              : <Checkbox label={onto ? `Push ${b.targetBranch.trim() || "the branch"} to origin when fixed (pulled from origin first)` : "Push the ticket's branch to origin when fixed"} checked={b.push} onChange={(v) => set({ push: v })} />}
            <div className="flex items-center gap-2">
              <Checkbox label="Watch the column: work on new tickets every" checked={b.watch} onChange={(v) => set({ watch: v })} />
              <Input type="number" min={1} className="w-16" value={b.intervalMin} onChange={(e) => set({ intervalMin: Number(e.target.value) || 10 })} />
              <span className="text-[13px]">min, up to</span>
              <Input type="number" min={1} max={20} className="w-14" value={b.maxPerRun} onChange={(e) => set({ maxPerRun: Number(e.target.value) || 3 })} />
              <span className="text-[13px]">at a time</span>
            </div>
          </div>
          {b.allowCommands && <p className="text-[11px] text-warn">The agent will run any command it decides to, in the repository, as you. Only allow this for code and tickets you trust.</p>}
        </section>

        <section className="space-y-3 border-t border-line pt-4">
          <div className="text-xs font-semibold tracking-wide text-muted uppercase">Slack</div>
          <Field label="Incoming webhook" hint="Posts each result to a channel (Mattermost and Google Chat webhooks work too). Kept in the keychain.">
            <div className="flex gap-2">
              <Input type="password" value={webhook ?? ""} onChange={(e) => setWebhook(e.target.value)} placeholder={b.hasWebhook ? "Saved (type to replace, clear to remove)" : "https://hooks.slack.com/services/…"} />
              <Button disabled={!(webhook?.trim() || (b.hasWebhook && webhook === undefined))} onClick={() => void tickets.testWebhook(withSecrets(b)).then(() => toast.success("Sent a test message")).catch((e) => setError(errMsg(e)))}>Test</Button>
            </div>
          </Field>
          {b.hasWebhook && webhook === undefined && (
            <button className="text-[11px] text-muted hover:text-danger" onClick={() => setWebhook("")}>Remove the saved webhook</button>
          )}
        </section>
        {error && <ErrorBox error={error} />}
      </div>
    </Dialog>
  );
}
