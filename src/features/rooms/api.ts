// Rooms (src-tauri/src/rooms.rs, crates/rooms): people on their own networks share documents and a
// thread, and each person's DevCat Agent can join in.
// Events: "rooms-changed" ({ room, what }), "room-join-request" (JoinRequest),
// "room-join-resolved" ({ requestId }), "room-agent" ({ room, running, error? }).
import { invoke } from "@tauri-apps/api/core";

export type Role = "owner" | "editor" | "viewer" | "keeper";
export type Mentions = "anyone" | "me" | "off";

export interface RoomSummary { id: string; name: string; role: Role; members: number; online: number; removed: boolean }
export interface JoinRequest { requestId: string; room: string; roomName: string; member: string; name: string; role: Role }
export interface RoomsInfo { id: string; rooms: RoomSummary[]; pending: JoinRequest[] }

export interface ConnPath { direct: boolean; rttMs: number; relay?: string | null }
export interface MemberView { id: string; name: string; role: Role; me: boolean; online: boolean; file: string | null; typing: boolean; path: ConnPath | null }
export interface Invite { token: string; role: Role; createdAt: number; expiresAt: number; used: boolean }
export interface RoomSettings { name: string; agentEdits: boolean }
export interface Prefs { agentProvider?: string | null; mentions: Mentions }
export interface RoomView {
  summary: RoomSummary;
  settings: RoomSettings;
  me: string;
  owner: boolean;
  members: MemberView[];
  /** `@handle` of each member's agent, by member id. */
  handles: Record<string, string>;
  invites: Invite[];
  prefs: Prefs;
  agentRunning: boolean;
}

export interface Author { member: string; name: string; agent: boolean }
export interface FileInfo { path: string; kind: "text" | "blob"; createdBy: Author; createdAt: number; blob?: { hash: string; size: number; mime: string }; chars: number }
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
export interface RoomBoard { files: FileInfo[]; messages: Message[]; suggestions: Suggestion[]; claims: Claim[]; history: HistoryItem[] }

export const label = (a: Author) => (a.agent ? `${a.name}'s agent` : a.name);

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
  post: (room: string, text: string) => invoke<Message>("room_post", { room, text }),
  suggest: (room: string, path: string, find: string, replacement: string, note?: string) => invoke<void>("room_suggest", { room, path, find, replacement, note: note ?? null }),
  decide: (room: string, id: string, accept: boolean, reason?: string) => invoke<void>("room_decide", { room, id, accept, reason: reason ?? null }),
  revert: (room: string, change: string) => invoke<void>("room_revert", { room, change }),
  fileNew: (room: string, path: string, content?: string) => invoke<void>("room_file_new", { room, path, content: content ?? null }),
  fileDelete: (room: string, path: string) => invoke<void>("room_file_delete", { room, path }),
  /** Share a file from this computer; returns its name in the room. */
  fileAdd: (room: string, local: string, path?: string) => invoke<string>("room_file_add", { room, local, path: path ?? null }),
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
