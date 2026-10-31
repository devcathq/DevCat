// Agent bots (src-tauri/src/integrations): chat-app bots linked to folders, and notification
// webhooks. Desktop only: never through the phone dispatcher.
import { invoke } from "@tauri-apps/api/core";

export type BotKind = "telegram" | "discord";
export type ChannelKind = "discord" | "slack" | "webhook";

export interface Perms { notify: boolean; approve: boolean; requests: boolean; files: boolean; connections: boolean; autoEdit: boolean }
export const DEFAULT_PERMS: Perms = { notify: true, approve: true, requests: true, files: false, connections: false, autoEdit: false };

export interface BotStatus { id: string; kind: BotKind; name: string; enabled: boolean; running: boolean; error?: string | null; warning?: string | null }
export interface LinkUser { id: string; name: string }
export interface Link { id: string; botId: string; chatId: string; chatName: string; users: LinkUser[]; folder: string; providerId: string; perms: Perms; sessionId?: string | null; createdMs: number }
export interface Channel { id: string; kind: ChannelKind; name: string; enabled: boolean; onDone: boolean; onInput: boolean }
export interface LinkCode { code: string; botId: string; folder: string; providerId: string; perms: Perms; expiresMs: number }
export interface LinkRequest { requestId: string; botId: string; kind: BotKind; botName: string; chatId: string; chatName: string; private: boolean; user: LinkUser; folder: string; providerId: string; perms: Perms; expiresMs: number }
export interface IntegrationsStatus { bots: BotStatus[]; links: Link[]; channels: Channel[]; code?: LinkCode | null; requests: LinkRequest[] }

export const integrationsApi = {
  status: () => invoke<IntegrationsStatus>("integrations_status"),
  botAdd: (kind: BotKind, token: string) => invoke<IntegrationsStatus>("integrations_bot_add", { kind, token }),
  botEnable: (id: string, enabled: boolean) => invoke<IntegrationsStatus>("integrations_bot_enable", { id, enabled }),
  botRemove: (id: string) => invoke<IntegrationsStatus>("integrations_bot_remove", { id }),
  linkCode: (botId: string, folder: string, providerId: string, perms: Perms) => invoke<IntegrationsStatus>("integrations_link_code", { botId, folder, providerId, perms }),
  linkCancelCode: () => invoke<IntegrationsStatus>("integrations_link_cancel_code"),
  linkAnswer: (requestId: string, allow: boolean) => invoke<IntegrationsStatus>("integrations_link_answer", { requestId, allow }),
  linkUpdate: (id: string, perms: Perms, providerId: string | null) => invoke<IntegrationsStatus>("integrations_link_update", { id, perms, providerId }),
  linkRemove: (id: string) => invoke<IntegrationsStatus>("integrations_link_remove", { id }),
  linkTest: (id: string) => invoke<void>("integrations_link_test", { id }),
  channelAdd: (kind: ChannelKind, name: string, url: string) => invoke<IntegrationsStatus>("integrations_channel_add", { kind, name, url }),
  channelUpdate: (id: string, enabled: boolean, onDone: boolean, onInput: boolean) => invoke<IntegrationsStatus>("integrations_channel_update", { id, enabled, onDone, onInput }),
  channelRemove: (id: string) => invoke<IntegrationsStatus>("integrations_channel_remove", { id }),
  channelTest: (id: string) => invoke<void>("integrations_channel_test", { id }),
};
