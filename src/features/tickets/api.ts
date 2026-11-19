// Tickets (src-tauri/src/tickets): Jira, Trello, Linear and GitHub boards whose tickets the agent works on.
// Events: "tickets-job" ({ board, job | null }) while a board works, "tickets-changed" ({ board })
// when its history changes.
import { invoke } from "@tauri-apps/api/core";

export type TrackerKind = "jira" | "trello" | "linear" | "github";
/** Whose tickets the agent takes (the token owner = "me"). */
export type Assigned = "anyone" | "me" | "meOrNobody";
/** How a run gets its working copy: the repository itself, or a git worktree for each ticket. */
export type Isolation = "inPlace" | "worktree";

export interface Column { id: string; name: string }
export interface Ticket {
  id: string; key: string; title: string; description: string; url: string; labels: string[]; assignees: string[]; column: string;
  comments: { author: string; text: string }[];
}

export interface Board {
  /** "" = new. */
  id: string;
  name: string;
  kind: TrackerKind;
  /** Jira: the site URL. GitHub: the API address (https://api.github.com, GitHub Enterprise: https://<host>/api/v3). */
  site: string;
  /** Jira Cloud: email (empty = Data Center token). Trello: API key. */
  user: string;
  /** Jira: project key. Trello: board link or id. Linear: team key. GitHub: the repository, `owner/name`. */
  project: string;
  /** Write-only: a token to store, undefined = keep the saved one. */
  token?: string;
  hasToken?: boolean;
  /** Write-only: a webhook to store, "" = remove it, undefined = keep. */
  webhook?: string;
  hasWebhook?: boolean;
  /** GitHub: the token owner's login, read when the board is saved (branches are `<login>/<number>-<title>`). */
  login?: string;
  /** GitHub: logins whose trigger label the watch takes although they can't write to the repository (e.g. a triage bot). */
  allowlist: string[];
  /** When the board was first saved (ms; set by DevCat). A GitHub board's watch skips issues labelled before it. */
  created?: number;
  /** A git repository, or a folder of them. */
  repo: string;
  /** "worktree": each ticket in `<repo>/.worktrees`, made from origin; `targetBranch` is then the base branch. */
  isolation: Isolation;
  /** Worktree: paths (git ignores them, e.g. ".env") copied from `repo` into each new worktree. */
  copyFiles: string[];
  /** Worktree: run in each new worktree before the agent (only with `allowCommands`); a failure fails the run. */
  setupCommand: string;
  /** DevCat project whose memory and connections the agent gets (null: the one `repo` is in). */
  projectId?: string | null;
  providerId: string;
  model?: string | null;
  pickFrom?: Column | null;
  working?: Column | null;
  doneTo?: Column | null;
  /** Where a ticket goes when the agent asks questions (null: as when it couldn't finish). */
  needsInfoTo?: Column | null;
  failedTo?: Column | null;
  label: string;
  assigned: Assigned;
  comment: boolean;
  push: boolean;
  /** New branches are `<prefix><key>-<title>` (default "devcat/"). */
  branchPrefix: string;
  /** Commit fixed tickets onto this existing branch (main, staging…); "" = a new branch per ticket. */
  targetBranch: string;
  allowCommands: boolean;
  watch: boolean;
  intervalMin: number;
  maxPerRun: number;
  /** Tickets worked on at the same time (1 to 4), each in its own worktree; in place always 1. */
  parallelRuns: number;
  instructions: string;
}

/** A board's job: the tickets worked on now (several with parallel runs) and the ones waiting (ids). */
export interface JobView { runs: RunView[]; queue: string[]; started: number; stopping: boolean }
export interface RunView {
  ticketId: string; key?: string | null; title?: string | null; url?: string | null;
  stage: string; log: string[]; started: number;
}

export type Outcome = "fixed" | "needs_info" | "failed" | "error" | "stopped";
export interface RunRecord {
  ticketId: string; key: string; title: string; url: string; outcome: Outcome;
  summary: string; testing: string; branch?: string | null; filesChanged: number;
  /** A folder of repositories: the ones changed, "name (n files)". */
  repos: string[]; movedTo?: string | null;
  notes: string[]; started: number; finished: number; costUsd?: number | null;
  /** The prompt given for this ticket instead of the board's default one. */
  prompt?: string | null;
  /** The agent's chat (open it in the AI workspace): session id, AI connection, folder. */
  sessionId?: string | null; providerId?: string | null; cwd?: string | null;
  /** The person's answer to the agent's questions (from the Tickets chat) that started this run. */
  reply?: string | null;
  /** GitHub: the draft pull request DevCat opened for the run. */
  prNumber?: number | null; prUrl?: string | null;
}

export const ASSIGNED: Record<Assigned, string> = { anyone: "Anyone", me: "Me", meOrNobody: "Me, or nobody yet" };

export const TRACKERS: Record<TrackerKind, string> = { jira: "Jira", trello: "Trello", linear: "Linear", github: "GitHub" };

/** A GitHub board's state labels (its columns: a label's id is its name), as the triage skills name them. */
const label = (name: string): Column => ({ id: name, name });

/** What a new board of each kind starts with. GitHub: the default state labels, a worktree for each issue, push on. */
const KIND_DEFAULTS: Record<TrackerKind, Partial<Board>> = {
  jira: {}, trello: {}, linear: {},
  github: {
    site: "https://api.github.com", isolation: "worktree", push: true, branchPrefix: "",
    pickFrom: label("ready-for-agent"), working: label("agent-working"), doneTo: label("agent-review"), needsInfoTo: label("needs-info"), failedTo: label("ready-for-human"),
  },
};

export const newBoard = (kind: TrackerKind = "jira"): Board => ({
  id: "", name: "", kind, site: "", user: "", project: "", repo: "", projectId: null, providerId: "claude", model: null,
  pickFrom: null, working: null, doneTo: null, needsInfoTo: null, failedTo: null, label: "", assigned: "anyone",
  comment: true, push: false, branchPrefix: "devcat/", targetBranch: "", allowCommands: false, watch: false, intervalMin: 10, maxPerRun: 3, parallelRuns: 1, instructions: "",
  isolation: "inPlace", copyFiles: [], setupCommand: "", allowlist: [],
  ...KIND_DEFAULTS[kind],
});

export const tickets = {
  boards: () => invoke<Board[]>("tickets_boards"),
  save: (board: Board) => invoke<Board>("tickets_board_save", { board }),
  remove: (id: string) => invoke<void>("tickets_board_delete", { id }),
  columns: (board: Board) => invoke<Column[]>("tickets_columns", { board }),
  list: (boardId: string, column: string) => invoke<Ticket[]>("tickets_list", { boardId, column }),
  /** `prompt`: what the agent is told for these tickets instead of the board's default prompt. */
  fix: (boardId: string, ticketIds: string[], prompt?: string | null) => invoke<void>("tickets_fix", { boardId, ticketIds, prompt: prompt ?? null }),
  /** Answer the agent's questions about a ticket: it goes on from its last run (same chat, same branch). */
  reply: (boardId: string, ticketId: string, text: string) => invoke<void>("tickets_reply", { boardId, ticketId, text }),
  stop: (boardId: string) => invoke<void>("tickets_stop", { boardId }),
  jobs: () => invoke<Record<string, JobView>>("tickets_jobs"),
  history: (boardId: string) => invoke<RunRecord[]>("tickets_history", { boardId }),
  testWebhook: (board: Board) => invoke<void>("tickets_webhook_test", { board }),
};
