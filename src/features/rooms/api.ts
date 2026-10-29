// Rooms (src-tauri/src/rooms.rs, crates/rooms): people on their own networks share documents and a
// thread, and each person's DevCat Agent can join in.
// Events: "rooms-changed" ({ room, what }), "room-join-request" (JoinRequest),
// "room-join-resolved" ({ requestId }), "room-agent" ({ room, running, error? }),
// "room-mention" ({ room, roomName, message }: someone named this user).
import { invoke } from "@tauri-apps/api/core";

export type Role = "owner" | "editor" | "viewer" | "keeper";
export type Mentions = "anyone" | "me" | "off";

export interface RoomSummary { id: string; name: string; role: Role; members: number; online: number; removed: boolean }
export interface JoinRequest { requestId: string; room: string; roomName: string; member: string; name: string; role: Role }
/** A room this DevCat was removed from, and so deleted. */
export interface Gone { id: string; name: string }
/** `gone`: rooms deleted since the window last asked (told once). */
export interface RoomsInfo { id: string; rooms: RoomSummary[]; pending: JoinRequest[]; gone: Gone[] }

export interface ConnPath { direct: boolean; rttMs: number; relay?: string | null }
export interface MemberView { id: string; name: string; role: Role; me: boolean; online: boolean; file: string | null; typing: boolean; path: ConnPath | null }
export interface Invite { token: string; role: Role; createdAt: number; expiresAt: number; used: boolean }
export interface RoomSettings { name: string; agentEdits: boolean }
/** `codingAgents`: this user's Claude Code, Codex… (devcat mcp) and DevCat Agent chats may use the room. */
export interface Prefs { agentProvider?: string | null; mentions: Mentions; codingAgents?: boolean }
export interface RoomView {
  summary: RoomSummary;
  settings: RoomSettings;
  me: string;
  owner: boolean;
  members: MemberView[];
  /** `@handle` of each member's agent, by member id. */
  handles: Record<string, string>;
  /** `@handle` of each member themselves, by member id. */
  people: Record<string, string>;
  invites: Invite[];
  prefs: Prefs;
  agentRunning: boolean;
}

/** `via`: the coding agent that acted for the person from outside the room (Claude Code, Codex…). */
export interface Author { member: string; name: string; agent: boolean; via?: string | null }
/** `chars`: characters of a text file, shapes of a whiteboard. */
export interface FileInfo { path: string; kind: "text" | "blob" | "board"; createdBy: Author; createdAt: number; blob?: { hash: string; size: number; mime: string }; chars: number }
/** A file just shared: its name in the room, and what to tell the person (big text went in as a plain file). */
export interface SharedFile { name: string; note: string | null }
export interface Message { id: string; author: Author; text: string; at: number; depth: number; replyTo?: string }
export interface Suggestion {
  id: string; path: string; author: Author; at: number; note?: string;
  base: string; replacement: string; state: "open" | "accepted" | "rejected";
  decidedBy?: Author; decidedAt?: number; reason?: string;
  /** Where the range is now, in characters (open suggestions). */
  from?: number; to?: number;
  outdated: boolean;
}
export interface Claim { id: string; path: string; from: number; to: number; text: string; holder: Author; at: number; expires: number; winning: boolean; note?: string }
export interface HistoryItem {
  id: string; kind: "suggestion" | "edit" | "revert" | "newFile"; path: string; by: Author; approvedBy?: Author; at: number;
  before: string; after: string; suggestion?: string; reverts?: string; revertible: boolean;
}
export type TaskState = "open" | "doing" | "done";
export interface Assignee { member: string; agent: boolean }
export interface Task {
  id: string; title: string; notes?: string | null; assignee?: Assignee | null; state: TaskState;
  createdBy: Author; createdAt: number; updatedBy?: Author | null; updatedAt: number;
}
/** Fields left out stay; `null` clears notes, or makes the task anyone's. */
export interface TaskPatch { title?: string; notes?: string | null; assignee?: Assignee | null; state?: TaskState }
export interface Pin { message: string; by: Author; at: number }
export interface Reaction { message: string; emoji: string; by: Author; at: number }
/** Something someone did with a file or folder: shared it in (`from` where), made, moved (`to` its
 *  new name), deleted, or took it out (`to`: their computer, skills, memory, notes…). */
export type ActivityKind = "shared" | "created" | "moved" | "deleted" | "tookOut";
export interface Activity { id: string; kind: ActivityKind; path: string; from?: string | null; to?: string | null; files?: number | null; by: Author; at: number }
/** A stretch of someone's edits to a file (from the document's own history). */
export interface EditView { who: string; member?: string | null; agent: boolean; from: number; to: number; inserted: number; deleted: number; changes: number }
export interface FileHistory { names: string[]; activity: Activity[]; edits: EditView[] }
export interface RoomBoard { files: FileInfo[]; messages: Message[]; suggestions: Suggestion[]; claims: Claim[]; history: HistoryItem[]; tasks: Task[]; pins: Pin[]; reactions: Reaction[]; activity: Activity[] }
/** A skill on this computer: `dir`/SKILL.md; `from`: "~" (every project) or a project folder. */
export interface SkillInfo { name: string; description: string; dir: string; from: string }
/** What a message reacts with (crates/rooms REACTION_EMOJI). */
export const REACTIONS = ["👍", "❤️", "🎉", "😄", "🤔", "👀", "✅"];

export const label = (a: Author) => (a.agent ? `${a.name}'s agent${a.via ? ` (${a.via})` : ""}` : a.name);

export const rooms = {
  /** `start` false: only if rooms are already running (doesn't open the rooms socket). */
  info: (start = true) => invoke<RoomsInfo>("rooms_info", { start }),
  create: (name: string, myName: string) => invoke<string>("room_create", { name, myName }),
  invite: (room: string, role: Role) => invoke<string>("room_invite", { room, role }),
  /** Waits for the owner's answer (up to 5 minutes). */
  join: (link: string, myName: string) => invoke<string>("room_join", { link, myName }),
  answer: (requestId: string, role: Role | null) => invoke<void>("room_join_answer", { requestId, role }),
  forget: (room: string) => invoke<void>("room_forget", { room }),
  view: (room: string) => invoke<RoomView>("room_view", { room }),
  board: (room: string) => invoke<RoomBoard>("room_board", { room }),
  /** What a copy at `version` (base64url, "" for none) lacks. */
  docUpdates: (room: string, version: string) => invoke<{ updates: string; version: string }>("room_doc_updates", { room, version }),
  docImport: (room: string, updates: string, peer: string) => invoke<void>("room_doc_import", { room, updates, peer }),
  post: (room: string, text: string, replyTo?: string) => invoke<Message>("room_post", { room, text, replyTo: replyTo ?? null }),
  /** Returns whether the reaction is on now (a second click takes it back). */
  react: (room: string, message: string, emoji: string) => invoke<boolean>("room_react", { room, message, emoji }),
  /** Pin a message as a decision (`pin` false: unpin). */
  pin: (room: string, message: string, pin: boolean) => invoke<void>("room_pin", { room, message, pin }),
  taskAdd: (room: string, title: string, notes?: string, assignee?: Assignee | null) => invoke<Task>("room_task_add", { room, title, notes: notes ?? null, assignee: assignee ?? null }),
  taskUpdate: (room: string, id: string, patch: TaskPatch) => invoke<Task>("room_task_update", { room, id, patch }),
  taskDelete: (room: string, id: string) => invoke<void>("room_task_delete", { room, id }),
  suggest: (room: string, path: string, find: string, replacement: string, note?: string) => invoke<void>("room_suggest", { room, path, find, replacement, note: note ?? null }),
  decide: (room: string, id: string, accept: boolean, reason?: string) => invoke<void>("room_decide", { room, id, accept, reason: reason ?? null }),
  revert: (room: string, change: string) => invoke<void>("room_revert", { room, change }),
  fileNew: (room: string, path: string, content?: string) => invoke<void>("room_file_new", { room, path, content: content ?? null }),
  fileDelete: (room: string, path: string) => invoke<void>("room_file_delete", { room, path }),
  /** Returns how many files it held. */
  folderDelete: (room: string, path: string) => invoke<number>("room_folder_delete", { room, path }),
  /** Rename or move a file or a folder; returns each [old, new] path. */
  rename: (room: string, from: string, to: string) => invoke<[string, string][]>("room_file_rename", { room, from, to }),
  boardNew: (room: string, path: string) => invoke<void>("room_board_new", { room, path }),
  /** Share text from DevCat (a note, a memory, a diff) as `name` in `folder`, or the next free name. */
  /** `from`: what it was ("a note"…), for the room's activity. */
  shareText: (room: string, name: string, text: string, folder?: string, from?: string) => invoke<SharedFile>("room_share_text", { room, name, text, folder: folder ?? null, from: from ?? null }),
  /** Share files and folders from this computer into `folder` of the room (`from`: where, for the room's activity). */
  pathsAdd: (room: string, paths: string[], folder?: string, from?: string) => invoke<SharedFile[]>("room_paths_add", { room, paths, folder: folder ?? null, from: from ?? null }),
  /** `to`: what the room's activity says ("their computer" by default). */
  fileSave: (room: string, path: string, dest: string, to?: string) => invoke<void>("room_file_save", { room, path, dest, to: to ?? null }),
  /** Save a room folder into `dest`; `replace`: also when `dest` has files. Returns how many. */
  folderSave: (room: string, path: string, dest: string, replace = false, to?: string) => invoke<number>("room_folder_save", { room, path, dest, replace, to: to ?? null }),
  /** Tell the room this user took a file or folder out (to their memory, notes, the clipboard…). */
  tookOut: (room: string, path: string, to: string, files?: number) => invoke<void>("room_took_out", { room, path, to, files: files ?? null }),
  /** Who shared, moved, took out and edited a file. */
  fileHistory: (room: string, path: string) => invoke<FileHistory>("room_file_history", { room, path }),
  skillsList: (folders: string[]) => invoke<SkillInfo[]>("skills_list", { folders }),
  /** Share a file from this computer; returns its name in the room. */
  fileAdd: (room: string, local: string, path?: string) => invoke<SharedFile>("room_file_add", { room, local, path: path ?? null }),
  /** Share a dropped or pasted file (`data`: its bytes, base64). */
  fileAddBytes: (room: string, name: string, data: string, folder?: string, from?: string) => invoke<SharedFile>("room_file_add_bytes", { room, name, data, folder: folder ?? null, from: from ?? null }),
  blob: (room: string, hash: string) => invoke<string>("room_blob", { room, hash }),
  blobSave: (room: string, hash: string, dest: string) => invoke<void>("room_blob_save", { room, hash, dest }),
  memberRemove: (room: string, member: string) => invoke<void>("room_member_remove", { room, member }),
  memberRole: (room: string, member: string, role: Role) => invoke<void>("room_member_role", { room, member, role }),
  settings: (room: string, s: { name?: string; agentEdits?: boolean }) => invoke<void>("room_settings", { room, name: s.name ?? null, agentEdits: s.agentEdits ?? null }),
  prefs: (room: string, prefs: Prefs) => invoke<void>("room_prefs", { room, prefs }),
  keeperAdd: (room: string, keeper: string, name?: string) => invoke<void>("room_keeper_add", { room, keeper, name: name ?? null }),
  focus: (room: string, file: string | null, typing: boolean) => invoke<void>("room_focus", { room, file, typing }),
  agentStop: (room: string) => invoke<void>("room_agent_stop", { room }),
};

/** base64url ↔ bytes (the backend's encoding for Loro updates). */
export function fromB64(s: string): Uint8Array {
  const b = atob(s.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((s.length + 3) % 4));
  const out = new Uint8Array(b.length);
  for (let i = 0; i < b.length; i++) out[i] = b.charCodeAt(i);
  return out;
}

export function toB64(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}
