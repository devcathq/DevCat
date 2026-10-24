// Tickets (src-tauri/src/tickets): Jira, Trello and Linear boards whose tickets the agent works on.
// Events: "tickets-job" ({ board, job | null }) while a board works, "tickets-changed" ({ board })
// when its history changes.
import { invoke } from "@tauri-apps/api/core";

export type TrackerKind = "jira" | "trello" | "linear";
/** Whose tickets the agent takes (the token owner = "me"). */
export type Assigned = "anyone" | "me" | "meOrNobody";

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
  /** Jira: the site URL. */
  site: string;
  /** Jira Cloud: email (empty = Data Center token). Trello: API key. */
  user: string;
  /** Jira: project key. Trello: board link or id. Linear: team key. */
  project: string;
  /** Write-only: a token to store, undefined = keep the saved one. */
  token?: string;
  hasToken?: boolean;
  /** Write-only: a webhook to store, "" = remove it, undefined = keep. */
  webhook?: string;
  hasWebhook?: boolean;
  /** A git repository, or a folder of them. */
  repo: string;
  providerId: string;
  model?: string | null;
  pickFrom?: Column | null;
  working?: Column | null;
  doneTo?: Column | null;
  failedTo?: Column | null;
  label: string;
  assigned: Assigned;
  comment: boolean;
  push: boolean;
  allowCommands: boolean;
  watch: boolean;
  intervalMin: number;
  maxPerRun: number;
  instructions: string;
}

export interface JobView {
  key?: string | null; title?: string | null; url?: string | null;
  stage: string; log: string[]; queue: string[]; started: number; stopping: boolean;
}

export type Outcome = "fixed" | "needs_info" | "failed" | "error" | "stopped";
export interface RunRecord {
  ticketId: string; key: string; title: string; url: string; outcome: Outcome;
  summary: string; testing: string; branch?: string | null; filesChanged: number;
  /** A folder of repositories: the ones changed, "name (n files)". */
  repos: string[]; movedTo?: string | null;
  notes: string[]; started: number; finished: number; costUsd?: number | null;
}

export const ASSIGNED: Record<Assigned, string> = { anyone: "Anyone", me: "Me", meOrNobody: "Me, or nobody yet" };

export const TRACKERS: Record<TrackerKind, string> = { jira: "Jira", trello: "Trello", linear: "Linear" };

export const newBoard = (kind: TrackerKind = "jira"): Board => ({
  id: "", name: "", kind, site: "", user: "", project: "", repo: "", providerId: "claude", model: null,
  pickFrom: null, working: null, doneTo: null, failedTo: null, label: "", assigned: "anyone",
  comment: true, push: false, allowCommands: false, watch: false, intervalMin: 10, maxPerRun: 3, instructions: "",
});

export const tickets = {
  boards: () => invoke<Board[]>("tickets_boards"),
  save: (board: Board) => invoke<Board>("tickets_board_save", { board }),
  remove: (id: string) => invoke<void>("tickets_board_delete", { id }),
  columns: (board: Board) => invoke<Column[]>("tickets_columns", { board }),
  list: (boardId: string, column: string) => invoke<Ticket[]>("tickets_list", { boardId, column }),
  fix: (boardId: string, ticketIds: string[]) => invoke<void>("tickets_fix", { boardId, ticketIds }),
  stop: (boardId: string) => invoke<void>("tickets_stop", { boardId }),
  jobs: () => invoke<Record<string, JobView>>("tickets_jobs"),
  history: (boardId: string) => invoke<RunRecord[]>("tickets_history", { boardId }),
  testWebhook: (board: Board) => invoke<void>("tickets_webhook_test", { board }),
};
