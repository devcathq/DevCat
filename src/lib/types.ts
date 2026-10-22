// Mirrors src-tauri/src/models.rs and src-tauri/src/db/mod.rs. Keep in sync.

export type ConnectionKind =
  | "postgres" | "mysql" | "sqlite" | "mssql" | "mongodb" | "redis"
  | "kubernetes" | "rabbitmq";

export const DATABASE_KINDS: ConnectionKind[] = ["postgres", "mysql", "sqlite", "mssql", "mongodb", "redis"];
export const INFRA_KINDS: ConnectionKind[] = ["kubernetes", "rabbitmq"];

export interface SshConfig {
  enabled: boolean;
  host: string;
  port: number;
  user: string;
  auth: "password" | "key";
  keyPath?: string | null;
  password?: string | null;
}

export interface ConnectionConfig {
  id: string;
  name: string;
  kind: ConnectionKind;
  host: string;
  port?: number | null;
  user: string;
  password?: string | null;
  /** Default db. Redis: db index. RabbitMQ: vhost. */
  database?: string | null;
  /** SQLite file, or kubeconfig path for Kubernetes. */
  filePath?: string | null;
  /** Full URI override (e.g. mongodb+srv://...). */
  url?: string | null;
  sslMode?: string | null;
  ssh?: SshConfig | null;
  color?: string | null;
  /** Legacy free-text group; migrated to projects by the backend. Don't use. */
  group?: string | null;
  /** Project (sidebar folder) this connection belongs to. */
  projectId?: string | null;
  /** Kubernetes: { context }. RabbitMQ: { managementPort, useTls }. */
  options: Record<string, string>;
  readOnly: boolean;
}

/** User-created folder that groups connections in the sidebar. */
/** Markdown note; projectId null = General (shown in every project). Times are Unix ms. */
export interface Note {
  id: string;
  projectId: string | null;
  body: string;
  pinned: boolean;
  createdAt: number;
  updatedAt: number;
}

export interface Project {
  id: string;
  name: string;
  color?: string | null;
  description?: string | null;
  order: number;
}

export type NodeKind =
  | "database" | "schema" | "folder" | "table" | "view" | "materializedView"
  | "function" | "procedure" | "trigger" | "sequence" | "index" | "foreignKey" | "column"
  | "event" | "collection" | "key" | "keyGroup" | "user";

export interface TableRef {
  database?: string | null;
  schema?: string | null;
  name: string;
}

export interface TreeNode {
  path: string[];
  label: string;
  kind: NodeKind;
  hasChildren: boolean;
  detail?: string | null;
  table?: TableRef | null;
}

/** Sidebar bookmark: one node of a connection's object tree. */
export interface Bookmark {
  id: string;
  connectionId: string;
  /** Tree path of the node, as passed to `children`. */
  path: string[];
  /** The node's label when it was bookmarked. */
  label: string;
  kind: NodeKind;
  /** What to open (database / schema / name), copied from the node. */
  table?: TableRef | null;
  order: number;
}

export interface ColumnInfo { name: string; typeName: string }

export type CellValue = string | number | boolean | null | object;

export interface QueryResult {
  statement: string;
  columns: ColumnInfo[];
  rows: CellValue[][];
  rowsAffected?: number | null;
  elapsedMs: number;
  truncated: boolean;
  message?: string | null;
}

export interface ExecOptions {
  database?: string | null;
  schema?: string | null;
  maxRows?: number | null;
  /** Don't record in query history (internal/background queries). Frontend-only flag. */
  skipHistory?: boolean;
}

export interface SortSpec { column: string; desc: boolean }

export interface TableDataRequest {
  table: TableRef;
  offset: number;
  limit: number;
  /** SQL WHERE expression, or Mongo JSON filter. */
  filter?: string | null;
  sort: SortSpec[];
}

export interface TableDataPage {
  result: QueryResult;
  total?: number | null;
  primaryKey: string[];
}

export interface ColumnDef {
  name: string;
  dataType: string;
  nullable: boolean;
  default?: string | null;
  isPrimaryKey: boolean;
  autoIncrement: boolean;
  comment?: string | null;
  ordinal: number;
}

export interface IndexDef {
  name: string;
  columns: string[];
  unique: boolean;
  primary: boolean;
  method?: string | null;
}

export interface ForeignKeyDef {
  name: string;
  columns: string[];
  refSchema?: string | null;
  refTable: string;
  refColumns: string[];
  onUpdate?: string | null;
  onDelete?: string | null;
}

export interface TableStructure {
  table?: TableRef | null;
  columns: ColumnDef[];
  indexes: IndexDef[];
  foreignKeys: ForeignKeyDef[];
  comment?: string | null;
}

export interface SchemaSnapshot {
  tables: TableStructure[];
  views: [TableRef, string][];
}

export type RowChange =
  | { op: "insert"; values: Record<string, CellValue> }
  | { op: "update"; key: Record<string, CellValue>; values: Record<string, CellValue> }
  | { op: "delete"; key: Record<string, CellValue> };

export type Dialect = "postgres" | "mysql" | "sqlite" | "mssql" | "none";

export function dialectOf(kind: ConnectionKind): Dialect {
  switch (kind) {
    case "postgres": case "mysql": case "sqlite": case "mssql": return kind;
    default: return "none";
  }
}

/** Redis key value as returned by db_get_key. */
export interface RedisKeyValue {
  key: string;
  type: "string" | "list" | "set" | "zset" | "hash" | "stream" | "none" | string;
  /** seconds, -1 = no expiry */
  ttl: number;
  length?: number;
  /** string: string; list/set: string[]; zset: [member, score][]; hash: Record<string,string>; stream: entries */
  value: unknown;
}

export interface HistoryEntry {
  id: string;
  connectionId: string;
  database?: string | null;
  query: string;
  executedAt: string;
  elapsedMs: number;
  success: boolean;
  error?: string | null;
}

export interface SavedQuery {
  id: string;
  name: string;
  connectionId?: string | null;
  database?: string | null;
  query: string;
  updatedAt: string;
}

export type ExportFormat = "csv" | "json" | "sql" | "xlsx";

// ---- Network profiles (mirror src-tauri/src/network/mod.rs)

/** "k8s" = Kubernetes port-forward(s) through one of DevCat's Kubernetes connections. */
export type NetKind = "ssh" | "openvpn" | "wireguard" | "k8s";

export interface PortForward {
  /** SSH: "local" = -L bindPort:remoteHost:remotePort ; "dynamic" = SOCKS5 proxy on bindPort.
   *  Kubernetes: "service" | "pod" named remoteHost in `namespace`. */
  kind: "local" | "dynamic" | "service" | "pod";
  /** 0 = pick a free port */
  bindPort: number;
  remoteHost: string;
  remotePort: number;
  /** Kubernetes forwards: "" = the connection's default namespace. */
  namespace?: string;
}

export interface NetProfile {
  id: string;
  name: string;
  kind: NetKind;
  projectId?: string | null;
  color?: string | null;
  autoConnect: boolean;
  ssh?: SshConfig | null;
  forwards: PortForward[];
  vpnUser?: string | null;
  /** write-only: sent on save, stored in the keychain */
  vpnPassword?: string | null;
  summary?: string | null;
  options: Record<string, string>;
}

export interface NetProfileFull { profile: NetProfile; configText?: string | null }

export type NetState = "disconnected" | "connecting" | "connected" | "disconnecting" | "error";

export interface NetStatus {
  id: string;
  state: NetState;
  since?: string | null;
  detail?: string | null;
  error?: string | null;
  localPorts: number[];
}

export interface NetTools {
  openvpn?: string | null;
  wireguard?: string | null;
  elevation: "osascript" | "pkexec" | "uac" | "none";
  installHints: Record<string, string>;
  /** Tools DevCat can install itself (Windows), with the version it would install. */
  installable?: Record<string, string>;
}

/** Passwordless VPN helper (mirror of network/helper.rs HelperStatus). */
export interface NetHelperStatus {
  /** macOS / Linux */
  supported: boolean;
  /** helper is root-owned and the sudoers rule exists */
  installed: boolean;
  /** `sudo -n` works without a prompt and the helper speaks this DevCat's protocol */
  upToDate: boolean;
  helperVersion?: string | null;
  /** this DevCat ships an OpenVPN the helper doesn't have yet (or an older copy): Update helper installs it */
  openvpnUpdate?: boolean;
  appVersion: string;
  helperPath: string;
  sudoersPath: string;
  /** the rule setup writes (null if the account name can't be used in sudoers) */
  sudoersLine?: string | null;
  /** why the helper can't run OpenVPN / WireGuard here (null = it can) */
  openvpnIssue?: string | null;
  wireguardIssue?: string | null;
}

export interface SshConfigHost { alias: string; host: string; port: number; user?: string | null; identityFile?: string | null }

/** One traffic sample (1/s) for a connected network profile; live via the "net-traffic" event. */
export interface TrafficSample {
  id: string;
  /** unix ms */
  at: number;
  /** bytes per second */
  rxRate: number;
  txRate: number;
  rxTotal: number;
  txTotal: number;
}

// ---- AI connections (mirror src-tauri/src/ai/mod.rs)

export type AiKind = "claude" | "codex" | "gemini" | "copilot" | "aider" | "opencode" | "custom" | "agent";
/** Model API of a DevCat Agent connection. */
/** A model a DevCat Agent API offers; `context` = its window in tokens, when known. */
export interface AgentModel { id: string; context: number | null }

export interface AgentSettings { protocol: "openai-chat" | "anthropic"; baseUrl: string; model: string }
export interface AiEnvVar { key: string; value: string }
/** Saved AI connection: the coding-agent CLI the AI workspace terminal runs. */
export interface AiProvider {
  /** "" = new. "claude" = the built-in Claude Code profile. */
  id: string;
  name: string;
  kind: AiKind;
  /** Program override (name on PATH or path); null = the kind's default binary. */
  command?: string | null;
  args: string[];
  env: AiEnvVar[];
  /** Variable the API key is passed in (null = the kind's default). */
  apiKeyEnv?: string | null;
  /** Write-only: a key to store in the keychain, "" = remove it, undefined = keep. */
  apiKey?: string | null;
  /** Read-only. */
  hasApiKey?: boolean;
  builtin?: boolean;
  /** DevCat Agent only. */
  agent?: AgentSettings | null;
}
export interface AiDetect {
  kind: AiKind;
  providerId?: string | null;
  program: string;
  /** null = not installed / not found. */
  path?: string | null;
  version?: string | null;
  installHint?: string | null;
  error?: string | null;
}

// ---- Claude Code workspace (mirror src-tauri/src/claude/mod.rs)

export type ClaudePermissionMode = "manual" | "acceptEdits" | "auto" | "plan" | "dontAsk" | "bypassPermissions";

export interface ClaudeInfo { path?: string | null; version?: string | null; account?: unknown; models?: unknown }
export interface ClaudeStartOptions { model?: string | null; permissionMode?: ClaudePermissionMode | null; resume?: string | null; fork?: boolean }
export interface ClaudeProject { cwd: string; sessionCount: number; lastActive?: string | null }
export interface SessionMeta {
  id: string; title: string; firstPrompt?: string | null; lastPrompt?: string | null;
  createdAt?: string | null; updatedAt?: string | null; messageCount: number; gitBranch?: string | null; costUsd?: number | null;
}
/** Raw stream-json object from the CLI (system / assistant / user / stream_event / result / control_*). */
export type ClaudeMsg = { type: string; subtype?: string; [k: string]: unknown };

export interface FileEntry { name: string; path: string; isDir: boolean; size: number; modified?: string | null; git?: string | null; ignored: boolean; /** directory is a nested git repo (has .git) */ isRepo?: boolean }
export interface SheetData { name: string; rows: string[][]; truncated: boolean }
export interface FilePreview {
  kind: "text" | "markdown" | "image" | "pdf" | "sheet" | "csv" | "docx" | "binary" | "too_large";
  path: string; mime?: string | null; size: number; text?: string | null; language?: string | null;
  dataBase64?: string | null; sheets?: SheetData[] | null; truncated: boolean;
  /** kind "text" only: may be edited and saved with fsWriteText */
  editable: boolean; readonlyReason?: string | null;
  /** on-disk version token for conflict detection (pass back to fsWriteText) */
  version?: string | null; lineEnding?: "lf" | "crlf" | null;
}
export interface GitFile { path: string; oldPath?: string | null; index: string; worktree: string }
export interface GitStatus { isRepo: boolean; root?: string | null; branch?: string | null; ahead: number; behind: number; files: GitFile[] }
export interface FileVersions { original: string; modified: string; binary: boolean }

// ---- git tools (mirror src-tauri/src/gitx/mod.rs)
export interface GitRepo {
  path: string; name: string; rel: string; parent?: string | null; isSubmodule: boolean;
  branch?: string | null; upstream?: string | null; ahead: number; behind: number; changes: number;
  headSha?: string | null; error?: string | null;
}
export interface GitLogQuery { rev?: string | null; all?: boolean; path?: string | null; search?: string | null; author?: string | null; skip?: number; limit?: number | null }
export type GitRefKind = "head" | "branch" | "remote" | "tag" | "detached" | "stash";
export interface GitRefLabel { name: string; kind: GitRefKind }
export interface GitCommit { sha: string; short: string; parents: string[]; author: string; email: string; time: number; subject: string; refs: GitRefLabel[] }
export interface GitCommitFile { path: string; oldPath?: string | null; status: string; additions?: number | null; deletions?: number | null; binary: boolean }
export interface GitCommitDetail { commit: GitCommit; body: string; committer: string; committerEmail: string; commitTime: number; files: GitCommitFile[] }
export interface BlameCommit { sha: string; author: string; email: string; time: number; summary: string; filename: string; uncommitted: boolean }
export interface BlameLine { line: number; sha: string; origLine: number }
export interface GitBlame { content: string; lines: BlameLine[]; commits: Record<string, BlameCommit> }
export interface GitBranch { name: string; fullRef: string; sha: string; current: boolean; upstream?: string | null; ahead: number; behind: number; subject: string; time: number }
export interface GitTag { name: string; sha: string; annotated: boolean; subject: string; time: number }
export interface GitRemote { name: string; url: string }
export interface GitRefs { current?: string | null; headSha?: string | null; local: GitBranch[]; remote: GitBranch[]; tags: GitTag[]; remotes: GitRemote[]; operation?: string | null }
export interface GitStash { index: number; name: string; sha: string; message: string; time: number }
export type CommitAction = "checkout" | "cherry-pick" | "revert" | "reset-soft" | "reset-mixed" | "reset-hard";
export interface RemoteOpts { remote?: string | null; rebase?: boolean; setUpstream?: boolean; force?: boolean; tags?: boolean }

// ---- app updates (mirror src-tauri/src/updater.rs)
export interface UpdateInfo { version: string; currentVersion: string; date?: string | null; notes?: string | null; canInstall: boolean }

// ---- keep awake (mirror src-tauri/src/keep_awake.rs)
export interface KeepAwakeState { on: boolean; until?: number | null }

// ---- DevCat Agent (crates/harness) ----
export type AgentPolicy = "ask" | "auto-edit" | "bypass";
export type AgentDecision = { decision: "allow" } | { decision: "allow-for-session" } | { decision: "deny"; reason?: string | null };
export interface AgentUsage { input_tokens: number; output_tokens: number; cache_read_tokens: number }
export type AgentBlock =
  | { type: "text"; text: string }
  | { type: "thinking"; text: string; signature?: string }
  | { type: "tool_use"; id: string; name: string; input: Record<string, unknown> }
  | { type: "tool_result"; tool_use_id: string; content: string; is_error?: boolean }
  | { type: "image"; media_type: string; data: string };
/** One line of a session log. */
export type AgentSessionEvent =
  | { event: "meta"; id: string; cwd: string; created_ms: number }
  | { event: "user"; text: string; images?: AgentBlock[] }
  | { event: "assistant"; content: AgentBlock[]; model: string; usage: AgentUsage }
  | { event: "tool_results"; results: AgentBlock[] }
  | { event: "approval"; tool_use_id: string; tool: string; allowed: boolean; by: string }
  | { event: "interrupted"; reason: string }
  | { event: "title"; title: string }
  | { event: "compacted"; summary: string; model: string; usage: AgentUsage; tokens_before: number };
export interface AgentContext { rules: string[]; skills: string[]; mcp: { name: string; tools: number }[]; notices: string[]; mcpLoading: boolean }
export interface AgentOpened { sessionId: string; title: string; model: string; providerName: string; busy: boolean; events: AgentSessionEvent[]; usage: AgentUsage; context: AgentContext }
export interface AgentQuestion { toolUseId: string; question: string; options: string[] }
export interface AgentApprovalRequest { toolUseId: string; tool: string; kind: "read" | "edit" | "edit-outside" | "exec" | "fetch"; summary: string; input: Record<string, unknown> }
/** Payload of `agent-event`. */
export type AgentLiveEvent =
  | { type: "turnStart"; text: string; attachments?: string[] }
  | { type: "question"; question: AgentQuestion }
  | { type: "context"; context: AgentContext }
  | { type: "text"; text: string }
  | { type: "thinking"; text: string }
  | { type: "toolStart"; id: string; name: string }
  | { type: "step"; content: AgentBlock[]; usage: AgentUsage }
  | { type: "compacting"; tokens_before: number }
  | { type: "compacted"; summary: string; tokens_before: number; usage: AgentUsage }
  | { type: "approval"; request: AgentApprovalRequest }
  | { type: "toolCall"; id: string; name: string; summary: string; input: Record<string, unknown> }
  | { type: "toolResult"; id: string; content: string; is_error: boolean }
  | { type: "turnEnd"; end: "answered" | "stepLimit" | "stopped" | "error"; error: string | null; usage: AgentUsage; title: string; elapsedMs: number };
