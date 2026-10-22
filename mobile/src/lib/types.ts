// Shapes the computer sends (desktop types are shared with ../../../src/lib/types).
export type {
  CellValue, ColumnDef, ConnectionConfig, ConnectionKind, HistoryEntry, NetProfile, NetStatus, Project, QueryResult,
  RowChange, SavedQuery, SessionMeta, ClaudeProject, TableDataPage, TableRef, TableStructure, TrafficSample, TreeNode, AiProvider,
  AgentChatInfo,
} from "../../../src/lib/types";
import type { CellValue, TableRef } from "../../../src/lib/types";

export interface PtyInfo {
  id: string;
  cwd: string;
  program: string;
  agent: boolean;
  /** "desktop" | "phone:<name>" */
  origin: string;
  startedAt: number;
  cols: number;
  rows: number;
}

export interface AgentStatus {
  ptyId: string;
  cwd?: string | null;
  sessionId?: string | null;
  status: "working" | "idle" | "waiting";
  since: number;
  message?: string | null;
  lastTurnMs?: number | null;
}

export interface HostInfo { id: string; name: string; os: string; appVersion: string; addrs: string[] }

export interface AuditEntry { at: number; device: string; deviceName: string; action: string; detail?: string | null; ok: boolean }

export interface NotifySettings {
  enabled: boolean; server: string; topic: string; token?: string | null;
  onDone: boolean; onInput: boolean; minSeconds: number; onlyWhenAway: boolean;
}

export interface K8sObject {
  kind?: string;
  metadata: { name: string; namespace?: string; creationTimestamp?: string; labels?: Record<string, string> };
  spec?: any;
  status?: any;
}

export type TabRoot = "agents" | "fileshome" | "data" | "network" | "cluster" | "more";

export type Route =
  | { s: "loading" }
  | { s: "welcome" }
  | { s: "weblanding"; link: string }
  | { s: "scan" }
  | { s: "paste"; link?: string }
  | { s: "pairing"; link: string; addr?: string | null }
  | { s: "computers" }
  | { s: TabRoot }
  | { s: "terminal"; id: string }
  | { s: "newchat"; cwd?: string }
  | { s: "sessions"; cwd?: string }
  | { s: "transcript"; cwd: string; sessionId: string; title: string }
  /** A DevCat Agent chat: `sessionId` null = a new one; `first` = a message to send right away. */
  | { s: "agentchat"; cwd: string; providerId: string; sessionId: string | null; first?: string }
  | { s: "browse"; connId: string; path: string[]; title: string }
  | { s: "rows"; connId: string; table: TableRef }
  | { s: "row"; connId: string; table: TableRef; row: Record<string, CellValue> | null; pk: string[] }
  | { s: "query"; connId: string; database?: string | null; schema?: string | null; sql?: string }
  | { s: "rediskey"; connId: string; database: string | null; key: string | null }
  | { s: "netlog"; id: string; name: string }
  | { s: "pod"; connId: string; namespace: string; name: string }
  | { s: "notify" }
  | { s: "help" }
  | { s: "activity" }
  | { s: "machine" }
  /** `root`: the project folder it was opened from (paths are copied relative to it). */
  | { s: "files"; dir: string; root?: string }
  | { s: "file"; path: string; root?: string };
